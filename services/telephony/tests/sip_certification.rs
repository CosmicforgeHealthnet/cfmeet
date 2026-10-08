//! REAL SIP / media edge certification (Part E §41, §54–§55, §220).
//!
//! Boots the selected edge (Asterisk, `infra/telephony/certification/`) on loopback and
//! drives Telephony's real ARI adapter, its SIP-trunk interconnect adapter and the
//! Telephony service against SIPp acting as carriers and as the application-media side:
//! outbound INVITE / 180 / 200 / ACK, real RTP (media evidence from the edge's RTP
//! statistics), application-media bridge, DTMF both ways, remote and edge hold / resume,
//! hangup, busy, no answer, inbound INVITE (ringing before answer), phone-to-phone
//! bridge, the redirect (transfer) primitive, unknown-peer refusal and edge-down safety.
//!
//! Requires `asterisk` and `sipp` (Ubuntu: `apt install asterisk sip-tester`). Without
//! them the test prints SKIPPED — unless `TELEPHONY_SIP_CERT_REQUIRED=1` (CI), where a
//! missing edge FAILS. The SIP peers are SIPp simulators: this certifies the edge and the
//! SIP signalling, never a real carrier.

mod common;

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::http::{Method, StatusCode};
use common::{Harness, RELAY_NUMBER};
use serde_json::{json, Value};
use telephony::{
    ports::InboundMapping,
    sip::{
        ari::{AriEdge, AriSettings},
        SipMediaEdge,
    },
};
use uuid::Uuid;

const ARI_URL: &str = "http://127.0.0.1:8088";
const ARI_USER: &str = "cfmeet-cert";
const ARI_PASSWORD: &str = "certification-only-ari-password-0123456789";
const EDGE: &str = "127.0.0.1:5070";

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

struct Asterisk {
    child: Child,
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
        let run = std::env::temp_dir().join(format!("cfmeet-sip-cert-{}", Uuid::new_v4().simple()));
        for d in ["etc", "varlib", "spool", "run", "log", "agi", "moh"] {
            std::fs::create_dir_all(run.join(d)).unwrap();
        }
        // One second of a 425 Hz hold tone, signed linear 8 kHz (".sln").
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
}

struct Sipp {
    child: Child,
    name: String,
    log_dir: PathBuf,
}

impl Sipp {
    /// Runs one SIPp scenario. `target` = remote host for UAC scenarios.
    fn start(
        scenario: &str,
        ip: &str,
        port: u16,
        media_port: u16,
        target: Option<&str>,
        keys: &[(&str, &str)],
    ) -> Self {
        let log_dir = std::env::temp_dir().join(format!("cfmeet-sipp-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&log_dir).unwrap();
        let mut cmd = Command::new("sipp");
        if let Some(t) = target {
            cmd.arg(t);
        }
        cmd.args([
            "-sf",
            cert_dir().join("sipp").join(scenario).to_str().unwrap(),
            "-i",
            ip,
            "-p",
            &port.to_string(),
            "-mi",
            ip,
            "-min_rtp_port",
            &media_port.to_string(),
            "-max_rtp_port",
            &(media_port + 98).to_string(),
            "-m",
            "1",
            "-l",
            "1",
            "-rtp_echo",
            "-timeout",
            "90s",
            "-timeout_error",
            "-nostdin",
            "-trace_err",
            "-trace_msg",
        ])
        .current_dir(&log_dir)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log_dir.join("stderr.log")).unwrap());
        for (k, v) in keys {
            cmd.args(["-key", k, v]);
        }
        let child = cmd.spawn().expect("sipp starts");
        Self {
            child,
            name: scenario.to_string(),
            log_dir,
        }
    }

    /// Waits for the scenario to finish; true = SIPp reported success.
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
        let mut out = format!("--- sipp {} ---\n", self.name);
        if let Ok(entries) = std::fs::read_dir(&self.log_dir) {
            for e in entries.flatten() {
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

impl Drop for Sipp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn edge_settings() -> AriSettings {
    AriSettings {
        base_url: ARI_URL.into(),
        username: ARI_USER.into(),
        password: zeroize::Zeroizing::new(ARI_PASSWORD.into()),
        app: "cfmeet-cert".into(),
        request_timeout: Duration::from_secs(5),
    }
}

/// Drives workers (and the wall clock) until `done` holds.
async fn until<F, Fut>(h: &Harness, what: &str, within: Duration, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    loop {
        h.clock.set(chrono::Utc::now());
        h.svc.run_commands_once(20).await.unwrap();
        h.svc.deliver_outbox_once(100).await.unwrap();
        if done().await {
            return;
        }
        if Instant::now() > deadline {
            let commands = h.text("SELECT string_agg(kind || ':' || status || ':' || coalesce(result_code, '-'), ' | ') FROM telephony_commands").await;
            let legs = h.text("SELECT string_agg(direction || ':' || status || ':' || coalesce(end_reason, '-'), ' | ') FROM telephony_legs").await;
            let events = h.text("SELECT string_agg(coalesce(normalized_event, '?') || ':' || disposition, ' | ' ORDER BY received_at) FROM provider_events").await;
            let console = std::fs::read_dir(std::env::temp_dir())
                .unwrap()
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("cfmeet-sip-cert-")
                })
                .filter_map(|e| std::fs::read_to_string(e.path().join("console.log")).ok())
                .last()
                .unwrap_or_default();
            let tail: Vec<&str> = console.lines().rev().take(40).collect();
            panic!(
                "timed out waiting for: {what}\ncommands: {commands:?}\nlegs: {legs:?}\nevents: {events:?}\nasterisk:\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn status_is(h: &Harness, leg: Uuid, statuses: &[&str]) -> bool {
    statuses.contains(&h.leg_status(leg).await.as_str())
}

async fn originate_to(h: &Harness, number: &str, routes: &[&str]) -> Uuid {
    originate_in(h, number, routes, None).await
}

async fn originate_in(h: &Harness, number: &str, routes: &[&str], call: Option<Uuid>) -> Uuid {
    let contact = h.privacy.add_contact(number, "NG");
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    body["candidateRoutes"] = json!(routes);
    if let Some(call) = call {
        body["call"]["callId"] = json!(call);
    }
    let r = h
        .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn bridge(h: &Harness, call_id: Uuid, kind: &str, legs: &[Uuid]) -> Value {
    let r = h
        .calling(
            Method::POST,
            "/internal/v1/bridges",
            Some(json!({
                "schemaVersion": "1",
                "commandId": Uuid::new_v4(),
                "idempotencyKey": format!("bridge-{}", Uuid::new_v4().simple()),
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                "callId": call_id,
                "kind": kind,
                "telephonyLegIds": legs,
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    r.json["bridge"].clone()
}

async fn call_id_of(h: &Harness, leg: Uuid) -> Uuid {
    h.text(&format!(
        "SELECT calling_call_id::text FROM telephony_legs WHERE id = '{leg}'"
    ))
    .await
    .unwrap()
    .parse()
    .unwrap()
}

async fn channel_of(h: &Harness, leg: Uuid) -> String {
    h.text(&format!(
        "SELECT provider_call_ref FROM provider_call_bindings WHERE telephony_leg_id = '{leg}' ORDER BY created_at DESC LIMIT 1"
    ))
    .await
    .unwrap()
}

async fn provider_event_count(h: &Harness, leg: Uuid, event: &str) -> i64 {
    h.scalar(&format!(
        "SELECT count(*) FROM provider_events WHERE telephony_leg_id = '{leg}' AND normalized_event = '{event}'"
    ))
    .await
}

async fn control(h: &Harness, leg: Uuid, action: &str) {
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{leg}/{action}"),
            Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(),
                         "idempotencyKey": format!("{action}-{}", Uuid::new_v4().simple()),
                         "correlationId": cf_service_auth::CorrelationId::generate().as_str() })),
        )
        .await;
    assert!(
        r.status == StatusCode::ACCEPTED || r.status == StatusCode::OK,
        "{action}: {}",
        r.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_sip_media_edge_certification() {
    let required = std::env::var("TELEPHONY_SIP_CERT_REQUIRED").as_deref() == Ok("1");
    if !(available("asterisk") && available("sipp")) {
        assert!(
            !required,
            "TELEPHONY_SIP_CERT_REQUIRED=1 but asterisk / sipp are not installed"
        );
        eprintln!("SKIPPED: SIP certification (asterisk / sipp not installed)");
        return;
    }
    if common::admin_url().is_none() {
        assert!(
            !required,
            "TELEPHONY_SIP_CERT_REQUIRED=1 but TEST_DATABASE_URL is not set"
        );
        eprintln!("SKIPPED: SIP certification (no TEST_DATABASE_URL)");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("telephony=warn"))
        .with_test_writer()
        .try_init();
    let asterisk = Asterisk::start();
    let edge = Arc::new(AriEdge::start(edge_settings()));
    // Wait for ARI and the event WebSocket (registers the application).
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if edge.health().await == telephony::domain::Health::Healthy {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the edge did not come up:\n{}",
            asterisk.console()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("edge: Asterisk ARI up, event stream connected");
    let mut h = Harness::with_edge(edge.clone()).await.unwrap();
    h.svc.settings.ring_timeout = Duration::from_secs(4);
    h.svc.settings.provider_request_timeout = Duration::from_secs(5);
    tokio::spawn(h.svc.clone().run_edge_events());

    // ---------------------------------------------------------------------------
    // A. Outbound: INVITE / 180 / 200 / ACK, application-media bridge, real RTP, DTMF
    //    both ways, remote + edge hold / resume, hangup, usage.
    // ---------------------------------------------------------------------------
    let mut carrier = Sipp::start(
        "carrier_uas_answer.xml",
        "127.0.0.2",
        5080,
        16000,
        None,
        &[],
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let leg = originate_to(&h, "+2348031234567", &["ng-a"]).await;
    until(
        &h,
        "outbound leg ringing",
        Duration::from_secs(10),
        || async { provider_event_count(&h, leg, "LEG_RINGING").await >= 1 },
    )
    .await;
    println!("A: outbound INVITE delivered, 180 Ringing -> LEG_RINGING");
    until(&h, "outbound leg answered", Duration::from_secs(10), || {
        status_is(&h, leg, &["ANSWERED", "CONNECTED", "HELD"])
    })
    .await;
    println!("A: 200 OK / ACK -> LEG_ANSWERED");
    let call = call_id_of(&h, leg).await;
    let b = bridge(&h, call, "APPLICATION_MEDIA", &[leg]).await;
    let code = b["mediaJoinCode"].as_str().expect("join code").to_string();
    let bridge_id = b["bridgeId"].as_str().unwrap().to_string();
    until(&h, "edge bridge created", Duration::from_secs(10), || async {
        h.scalar(&format!("SELECT count(*) FROM telephony_bridges WHERE id = '{bridge_id}' AND edge_bridge_ref IS NOT NULL")).await == 1
    })
    .await;
    let mut app_media = Sipp::start(
        "app_media_uac.xml",
        "127.0.0.3",
        5090,
        16100,
        Some(EDGE),
        &[("code", &code)],
    );
    until(&h, "application media joined; bridge active", Duration::from_secs(15), || async {
        h.scalar(&format!("SELECT count(*) FROM telephony_bridges WHERE id = '{bridge_id}' AND status = 'ACTIVE'")).await == 1
    })
    .await;
    assert!(status_is(&h, leg, &["CONNECTED", "HELD"]).await);
    println!(
        "A: application media joined with its single-use code -> BRIDGE_ESTABLISHED, LEG_CONNECTED"
    );
    let channel = channel_of(&h, leg).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let (rx, tx) = loop {
        if let Some((rx, tx)) = edge.media_stats(&channel).await {
            if rx > 0 && tx > 0 {
                break (rx, tx);
            }
        }
        assert!(
            Instant::now() < deadline,
            "no RTP media evidence on the public leg"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    println!("A: media anchored at the edge: public leg RTP rx={rx} tx={tx}");
    until(
        &h,
        "inbound DTMF from the carrier",
        Duration::from_secs(15),
        || async { provider_event_count(&h, leg, "LEG_DTMF").await >= 1 },
    )
    .await;
    edge.send_dtmf(&channel, "1#")
        .await
        .expect("DTMF towards the carrier");
    println!("A: DTMF received (RFC 4733 from the carrier) and sent (edge primitive)");
    until(
        &h,
        "remote hold and unhold",
        Duration::from_secs(20),
        || async {
            provider_event_count(&h, leg, "LEG_HELD").await >= 1
                && provider_event_count(&h, leg, "LEG_RESUMED").await >= 1
        },
    )
    .await;
    println!("A: remote hold / unhold (re-INVITE sendonly / sendrecv) -> LEG_HELD / LEG_RESUMED");
    until(
        &h,
        "connected after remote unhold",
        Duration::from_secs(5),
        || status_is(&h, leg, &["CONNECTED"]),
    )
    .await;
    control(&h, leg, "hold").await;
    until(&h, "edge hold", Duration::from_secs(10), || {
        status_is(&h, leg, &["HELD"])
    })
    .await;
    control(&h, leg, "resume").await;
    until(&h, "edge resume", Duration::from_secs(10), || {
        status_is(&h, leg, &["CONNECTED"])
    })
    .await;
    println!("A: hold / resume primitives through the edge");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    control(&h, leg, "terminate").await;
    until(&h, "terminated", Duration::from_secs(10), || {
        status_is(&h, leg, &["ENDED"])
    })
    .await;
    assert!(
        carrier.finished_ok(Duration::from_secs(15)).await,
        "{}",
        carrier.diagnostics()
    );
    assert!(
        app_media.finished_ok(Duration::from_secs(15)).await,
        "{}",
        app_media.diagnostics()
    );
    until(&h, "bridge ended", Duration::from_secs(10), || async {
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_bridges WHERE id = '{bridge_id}' AND status = 'ENDED'"
        ))
        .await
            == 1
    })
    .await;
    h.svc.finalize_usage_once(10).await.unwrap();
    let duration = h
        .scalar(&format!("SELECT connected_duration_ms FROM telephony_usage_records WHERE telephony_leg_id = '{leg}'"))
        .await;
    assert!(duration > 1000, "connected duration {duration} ms");
    println!(
        "A: hangup -> BYE / 200; bridge ended, application media released; usage {duration} ms"
    );

    // ---------------------------------------------------------------------------
    // B. Busy (486) and C. no answer (ring timeout -> CANCEL / 487).
    // ---------------------------------------------------------------------------
    let mut busy = Sipp::start("carrier_uas_busy.xml", "127.0.0.2", 5080, 16200, None, &[]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let leg = originate_to(&h, "+2348031234568", &["ng-a"]).await;
    until(&h, "busy", Duration::from_secs(10), || {
        status_is(&h, leg, &["FAILED"])
    })
    .await;
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "BUSY"
    );
    assert!(
        busy.finished_ok(Duration::from_secs(10)).await,
        "{}",
        busy.diagnostics()
    );
    println!("B: 486 Busy Here -> LEG_BUSY");
    let mut silent = Sipp::start(
        "carrier_uas_noanswer.xml",
        "127.0.0.2",
        5080,
        16300,
        None,
        &[],
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let leg = originate_to(&h, "+2348031234569", &["ng-a"]).await;
    until(&h, "no answer", Duration::from_secs(20), || {
        status_is(&h, leg, &["FAILED"])
    })
    .await;
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "NO_ANSWER"
    );
    assert!(
        silent.finished_ok(Duration::from_secs(10)).await,
        "{}",
        silent.diagnostics()
    );
    println!("C: 180 then nothing -> ring timeout -> CANCEL / 487 -> LEG_NO_ANSWER");

    // ---------------------------------------------------------------------------
    // D. Inbound: INVITE to the relay number from the carrier peer, ringing before
    //    answer, Calling offer, answer only when the application media joins, BYE.
    // ---------------------------------------------------------------------------
    h.privacy.mappings.lock().unwrap().insert(
        RELAY_NUMBER.to_string(),
        InboundMapping {
            decision_id: Uuid::new_v4(),
            context_id: Uuid::new_v4(),
            context_version: 1,
            relay_session_id: Uuid::new_v4(),
            relay_session_version: 1,
            assignment_id: Uuid::new_v4(),
            assignment_generation: 1,
            tenant_id: h.consumer.tenant_id,
            application_id: h.consumer.application_id,
            environment_id: h.consumer.environment_id,
            valid_until: chrono::Utc::now() + chrono::Duration::hours(1),
        },
    );
    let mut caller = Sipp::start(
        "carrier_uac_inbound.xml",
        "127.0.0.2",
        5081,
        16400,
        Some(EDGE),
        &[("called", RELAY_NUMBER), ("caller", "+2348035550001")],
    );
    until(&h, "inbound offered and ringing", Duration::from_secs(10), || async {
        h.scalar("SELECT count(*) FROM telephony_legs WHERE direction = 'INBOUND' AND status = 'RINGING'").await == 1
    })
    .await;
    let inbound: Uuid = h
        .text("SELECT id::text FROM telephony_legs WHERE direction = 'INBOUND'")
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(h.calling.offers.lock().unwrap().len(), 1);
    let offer = h.calling.offers.lock().unwrap()[0].to_string();
    assert!(
        !offer.contains("+234"),
        "the offer carries no number: {offer}"
    );
    println!("D: inbound INVITE -> peer validated -> 180 (not answered) -> relay mapping -> caller protected -> offer to Calling");
    let call = call_id_of(&h, inbound).await;
    let b = bridge(&h, call, "APPLICATION_MEDIA", &[inbound]).await;
    let code = b["mediaJoinCode"].as_str().unwrap().to_string();
    let bridge_id = b["bridgeId"].as_str().unwrap().to_string();
    until(&h, "inbound answered and bridge created", Duration::from_secs(10), || async {
        h.scalar(&format!("SELECT count(*) FROM telephony_bridges WHERE id = '{bridge_id}' AND edge_bridge_ref IS NOT NULL")).await == 1
            && status_is(&h, inbound, &["ANSWERED", "CONNECTED"]).await
    })
    .await;
    let mut app_media = Sipp::start(
        "app_media_uac.xml",
        "127.0.0.3",
        5090,
        16500,
        Some(EDGE),
        &[("code", &code)],
    );
    until(&h, "inbound bridged", Duration::from_secs(15), || {
        status_is(&h, inbound, &["CONNECTED"])
    })
    .await;
    println!("D: application endpoint accepted -> inbound answered (200 / ACK) -> bridged to application media");
    until(&h, "caller hung up", Duration::from_secs(20), || {
        status_is(&h, inbound, &["ENDED"])
    })
    .await;
    assert!(
        caller.finished_ok(Duration::from_secs(10)).await,
        "{}",
        caller.diagnostics()
    );
    assert!(
        app_media.finished_ok(Duration::from_secs(10)).await,
        "{}",
        app_media.diagnostics()
    );
    println!("D: caller BYE -> LEG_ENDED; bridge ended; application media released");

    // ---------------------------------------------------------------------------
    // E. Unknown peer refused by the edge; known peer to an unbound number refused
    //    by Telephony (SIP_PEER_REJECTED).
    // ---------------------------------------------------------------------------
    let mut stranger = Sipp::start(
        "unknown_peer_uac.xml",
        "127.0.0.9",
        5082,
        16600,
        Some(EDGE),
        &[("called", RELAY_NUMBER), ("caller", "+2348035550002")],
    );
    assert!(
        stranger.finished_ok(Duration::from_secs(15)).await,
        "{}",
        stranger.diagnostics()
    );
    let mut unbound = Sipp::start(
        "unknown_peer_uac.xml",
        "127.0.0.2",
        5083,
        16700,
        Some(EDGE),
        &[("called", "+2342019999999"), ("caller", "+2348035550003")],
    );
    assert!(
        unbound.finished_ok(Duration::from_secs(15)).await,
        "{}",
        unbound.diagnostics()
    );
    assert!(
        h.scalar("SELECT count(*) FROM telephony_incidents WHERE category = 'SIP_PEER_REJECTED'")
            .await
            >= 1
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_legs WHERE direction = 'INBOUND'")
            .await,
        1
    );
    println!("E: unidentified source refused at the edge; unbound called number refused by Telephony (incident)");

    // ---------------------------------------------------------------------------
    // F. Phone-to-phone bridge of two public legs on two interconnects.
    // ---------------------------------------------------------------------------
    let mut carrier_a = Sipp::start(
        "carrier_uas_answer_simple.xml",
        "127.0.0.2",
        5080,
        16800,
        None,
        &[],
    );
    let mut carrier_b = Sipp::start(
        "carrier_uas_answer_simple.xml",
        "127.0.0.4",
        5080,
        16900,
        None,
        &[],
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let call = Uuid::new_v4();
    let leg_a = originate_in(&h, "+2348031110001", &["ng-a"], Some(call)).await;
    let leg_b = originate_in(&h, "+2348031110002", &["ng-b"], Some(call)).await;
    until(
        &h,
        "both legs answered",
        Duration::from_secs(15),
        || async {
            status_is(&h, leg_a, &["ANSWERED", "CONNECTED", "HELD"]).await
                && status_is(&h, leg_b, &["ANSWERED", "CONNECTED", "HELD"]).await
        },
    )
    .await;
    // Legs of two different Calls can never be bridged together.
    let other_call = h.originate_body(Uuid::new_v4(), Uuid::new_v4(), 1)["call"]["callId"].clone();
    let r = h
        .calling(
            Method::POST,
            "/internal/v1/bridges",
            Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": "p2p-mixed-calls",
                         "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                         "callId": other_call, "kind": "PUBLIC_LEGS", "telephonyLegIds": [leg_a, leg_b] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let b = bridge(&h, call, "PUBLIC_LEGS", &[leg_a, leg_b]).await;
    assert!(b.get("mediaJoinCode").is_none());
    let bridge_id = b["bridgeId"].as_str().unwrap().to_string();
    until(&h, "phone-to-phone bridge active", Duration::from_secs(15), || async {
        h.scalar(&format!("SELECT count(*) FROM telephony_bridges WHERE id = '{bridge_id}' AND status = 'ACTIVE'")).await == 1
    })
    .await;
    for leg in [leg_a, leg_b] {
        let channel = channel_of(&h, leg).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if edge
                .media_stats(&channel)
                .await
                .is_some_and(|(rx, tx)| rx > 0 && tx > 0)
            {
                break;
            }
            assert!(Instant::now() < deadline, "no RTP on a bridged public leg");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    println!("F: two public legs (two interconnects) bridged at the edge with RTP on both; cross-call bridging refused");
    control(&h, leg_a, "terminate").await;
    until(&h, "bridge ends when a party leaves", Duration::from_secs(10), || async {
        h.scalar(&format!("SELECT count(*) FROM telephony_bridges WHERE id = '{bridge_id}' AND status = 'ENDED'")).await == 1
    })
    .await;
    control(&h, leg_b, "terminate").await;
    until(&h, "both ended", Duration::from_secs(10), || async {
        status_is(&h, leg_a, &["ENDED"]).await && status_is(&h, leg_b, &["ENDED"]).await
    })
    .await;
    assert!(
        carrier_a.finished_ok(Duration::from_secs(10)).await,
        "{}",
        carrier_a.diagnostics()
    );
    assert!(
        carrier_b.finished_ok(Duration::from_secs(10)).await,
        "{}",
        carrier_b.diagnostics()
    );
    println!("F: a party leaving ends the bridge; the other leg is terminated");

    // ---------------------------------------------------------------------------
    // G. Transfer primitive: a ringing inbound dialog is redirected (302).
    // ---------------------------------------------------------------------------
    let mut redirected = Sipp::start(
        "carrier_uac_redirected.xml",
        "127.0.0.2",
        5084,
        17000,
        Some(EDGE),
        &[("called", RELAY_NUMBER), ("caller", "+2348035550004")],
    );
    until(&h, "second inbound ringing", Duration::from_secs(10), || async {
        h.scalar("SELECT count(*) FROM telephony_legs WHERE direction = 'INBOUND' AND status = 'RINGING'").await == 1
    })
    .await;
    let ringing: Uuid = h
        .text("SELECT id::text FROM telephony_legs WHERE direction = 'INBOUND' AND status = 'RINGING'")
        .await
        .unwrap()
        .parse()
        .unwrap();
    let channel = channel_of(&h, ringing).await;
    edge.redirect(&channel, "PJSIP/peer-ng-b")
        .await
        .expect("redirect primitive");
    assert!(
        redirected.finished_ok(Duration::from_secs(15)).await,
        "{}",
        redirected.diagnostics()
    );
    until(&h, "redirected leg ended", Duration::from_secs(10), || {
        status_is(&h, ringing, &["ENDED", "FAILED"])
    })
    .await;
    println!("G: transfer primitive: ringing inbound dialog redirected (302)");

    // ---------------------------------------------------------------------------
    // H. Edge down: public routes fail safely, nothing is marked done.
    // ---------------------------------------------------------------------------
    drop(asterisk);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(edge.health().await, telephony::domain::Health::Unavailable);
    let leg = originate_to(&h, "+2348031234570", &["ng-a"]).await;
    until(&h, "fails safely", Duration::from_secs(10), || {
        status_is(&h, leg, &["FAILED"])
    })
    .await;
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "FAILED"
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM provider_call_bindings WHERE telephony_leg_id = '{leg}' AND status = 'BOUND'")).await,
        0
    );
    println!("H: edge down -> origination fails safely (no binding, nothing marked done)");
    let _ = &mut h;
    println!("SIP certification: PASSED");
}
