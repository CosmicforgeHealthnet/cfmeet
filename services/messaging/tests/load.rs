//! Load and resilience measurements (Part V §97). `#[ignore]`d: run explicitly with
//!
//! ```text
//! TEST_DATABASE_URL=… cargo test --release --test load -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Each test drives the REAL router (in-process HTTP, real PostgreSQL; real TCP WebSocket
//! clients for fan-out; real OpenMLS encryption on the client side for E2EE) and prints
//! one `LOAD {json}` line with its measurements. Nothing here is a capacity claim for
//! production hardware; the numbers describe the machine they ran on (recorded with the
//! results in docs/phases/phase-05/parts-e-g-evidence.md). No thresholds are asserted
//! beyond correctness (every message accepted, delivered once, in order).

mod common;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::{flows::*, realtime::*, *};
use futures_util::StreamExt;
use serde_json::json;

fn percentiles(mut v: Vec<Duration>) -> serde_json::Value {
    v.sort();
    let p = |q: f64| {
        let i = ((v.len() as f64 - 1.0) * q).round() as usize;
        v[i].as_secs_f64() * 1000.0
    };
    json!({ "p50_ms": p(0.50), "p95_ms": p(0.95), "p99_ms": p(0.99), "max_ms": p(1.0), "samples": v.len() })
}

fn report(name: &str, v: serde_json::Value) {
    println!("LOAD {}", json!({ "test": name, "result": v }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn managed_send_throughput() {
    let Some(h) =
        Harness::build(|s| s.conversations.environment_messages_per_minute = 1_000_000).await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 20).await;
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let per_sender = 50;
    let started = Instant::now();
    let futures = people.iter().map(|p| {
        let (h, c, conv) = (&h, &c, &conv);
        async move {
            let mut lat = Vec::new();
            for i in 0..per_sender {
                let t = Instant::now();
                let r = h
                    .send_managed(c, conv, *p, &format!("load message {i} from {p}"))
                    .await;
                assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
                lat.push(t.elapsed());
            }
            lat
        }
    });
    let lat: Vec<Duration> = futures_util::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect();
    let elapsed = started.elapsed();
    let total = lat.len();
    // Correctness: a gap-free canonical sequence.
    let max = h
        .scalar(&format!(
            "SELECT max(sequence) FROM messaging_messages WHERE conversation_id = '{}'",
            conv["conversationId"].as_str().unwrap()
        ))
        .await;
    let n = h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_messages WHERE conversation_id = '{}'",
            conv["conversationId"].as_str().unwrap()
        ))
        .await;
    assert_eq!(max, n);
    report(
        "managed_send_throughput",
        json!({
            "senders": people.len(), "messages": total, "seconds": elapsed.as_secs_f64(),
            "messages_per_second": total as f64 / elapsed.as_secs_f64(), "latency": percentiles(lat),
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn e2ee_send_throughput_with_real_mls() {
    let Some(h) =
        Harness::build(|s| s.conversations.environment_messages_per_minute = 1_000_000).await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({ "abuse": { "messagesPerMinute": 100000 } }))
        .await;
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let total = 500;
    let mut lat = Vec::new();
    let started = Instant::now();
    for i in 0..total {
        let t = Instant::now();
        let r = h
            .send_e2ee(
                &c,
                &conv,
                &mut alice,
                &format!("encrypted load message {i}"),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        lat.push(t.elapsed());
    }
    let elapsed = started.elapsed();
    let t = Instant::now();
    let mut got = Vec::new();
    loop {
        let page = h.receive(&c, &conv, &mut bob).await;
        if page.is_empty() {
            break;
        }
        got.extend(page);
    }
    let receive = t.elapsed();
    assert_eq!(got.len(), total);
    report(
        "e2ee_send_throughput_with_real_mls",
        json!({
            "messages": total, "seconds": elapsed.as_secs_f64(), "messages_per_second": total as f64 / elapsed.as_secs_f64(),
            "latency_including_client_encryption": percentiles(lat),
            "receive_and_decrypt_all_seconds": receive.as_secs_f64(),
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn realtime_fanout_latency() {
    let mut settings = test_settings();
    settings.revalidate_interval = Duration::from_secs(30);
    let Some(rt) = Rt::with(settings).await else {
        return;
    };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let receivers = 50;
    let (conv, people) = h.managed_group(&c, receivers + 1).await;
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let mut sockets = Vec::new();
    for p in &people[1..] {
        let m = Member {
            principal: *p,
            device: h.active_device(&c, *p).await,
            group: None,
            cursor: 0,
        };
        let mut ws = rt.connect(&c, &m).await;
        ws.subscribe(&conv, None).await;
        sockets.push(ws);
    }
    let messages = 100;
    // Receivers read concurrently from the start; every receiver gets every message
    // once, in order. Latency = start of the send request → receipt on the socket.
    let readers: Vec<_> = sockets
        .into_iter()
        .map(|mut ws| {
            tokio::spawn(async move {
                let mut out = Vec::new();
                let mut last = 0i64;
                while out.len() < messages {
                    let f = ws.next_of("event").await;
                    if f["event"]["data"]["message"].is_null() {
                        continue;
                    }
                    let seq = f["event"]["data"]["message"]["sequence"].as_i64().unwrap();
                    assert!(seq > last, "in order, no duplicates");
                    last = seq;
                    out.push((seq, Instant::now()));
                }
                out
            })
        })
        .collect();
    let started = Instant::now();
    let mut sent_at = Vec::new();
    for i in 0..messages {
        sent_at.push(Instant::now());
        let r = h
            .send_managed(&c, &conv, people[0], &format!("fanout {i}"))
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let mut lat = Vec::new();
    let futures = readers.into_iter().map(|t| async move { t.await.unwrap() });
    let all: Vec<Vec<(i64, Instant)>> = futures_util::future::join_all(futures).await;
    let first_seq = all[0][0].0;
    for per in &all {
        for (seq, at) in per {
            let i = (seq - first_seq) as usize;
            lat.push(at.saturating_duration_since(sent_at[i]));
        }
    }
    report(
        "realtime_fanout_latency",
        json!({
            "receivers": receivers, "messages": messages, "deliveries": lat.len(),
            "seconds": started.elapsed().as_secs_f64(), "send_to_receive": percentiles(lat),
        }),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn managed_search_latency() {
    let Some(h) =
        Harness::build(|s| s.conversations.environment_messages_per_minute = 1_000_000).await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 2).await;
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let words = [
        "invoice", "delivery", "meeting", "budget", "contract", "holiday", "report", "review",
    ];
    let n = 5000;
    let futures = (0..n).map(|i| {
        let (h, c, conv, p) = (&h, &c, &conv, people[i % 2]);
        let text = format!(
            "{} {} number {i}",
            words[i % words.len()],
            words[(i / 7) % words.len()]
        );
        async move {
            let r = h.send_managed(c, conv, p, &text).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        }
    });
    futures_util::stream::iter(futures)
        .buffer_unordered(50)
        .collect::<Vec<()>>()
        .await;
    let id = conv["conversationId"].as_str().unwrap();
    let mut lat = Vec::new();
    for i in 0..60 {
        let t = Instant::now();
        let r = h
            .get(
                &c,
                Some(people[0]),
                &format!(
                    "/v1/conversations/{id}/search?q={}&limit=50",
                    words[i % words.len()]
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert!(!r.json["results"].as_array().unwrap().is_empty());
        lat.push(t.elapsed());
    }
    report(
        "managed_search_latency",
        json!({ "indexed_messages": n, "queries": lat.len(), "latency": percentiles(lat) }),
    );
}

/// The same load spread over independent conversations (each conversation's canonical
/// order is a per-conversation lock; this separates that cost from overall throughput).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn managed_send_throughput_independent_conversations() {
    let Some(h) =
        Harness::build(|s| s.conversations.environment_messages_per_minute = 1_000_000).await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let mut convs = Vec::new();
    for _ in 0..20 {
        convs.push(h.managed_group(&c, 1).await);
    }
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "abuse": { "messagesPerMinute": 100000 } }),
    )
    .await;
    let per_sender = 50;
    let started = Instant::now();
    let futures = convs.iter().map(|(conv, people)| {
        let (h, c) = (&h, &c);
        async move {
            let mut lat = Vec::new();
            for i in 0..per_sender {
                let t = Instant::now();
                let r = h
                    .send_managed(c, conv, people[0], &format!("independent {i}"))
                    .await;
                assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
                lat.push(t.elapsed());
            }
            lat
        }
    });
    let lat: Vec<Duration> = futures_util::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect();
    let elapsed = started.elapsed();
    report(
        "managed_send_throughput_independent_conversations",
        json!({
            "conversations": convs.len(), "messages": lat.len(), "seconds": elapsed.as_secs_f64(),
            "messages_per_second": lat.len() as f64 / elapsed.as_secs_f64(), "latency": percentiles(lat),
        }),
    );
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

async fn connected_receivers(
    rt: &Rt,
    c: &Customer,
    conv: &serde_json::Value,
    people: &[uuid::Uuid],
) -> (Vec<Member>, Vec<Ws>) {
    let mut members = Vec::new();
    let mut sockets = Vec::new();
    for p in people {
        let m = Member {
            principal: *p,
            device: rt.h.active_device(c, *p).await,
            group: None,
            cursor: 0,
        };
        let mut ws = rt.connect(c, &m).await;
        ws.subscribe(conv, None).await;
        sockets.push(ws);
        members.push(m);
    }
    (members, sockets)
}

/// Fan-out well beyond 50 clients (`LOAD_FANOUT_RECEIVERS`, default 250): every receiver
/// gets every message once, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn realtime_large_fanout_latency() {
    let mut settings = test_settings();
    settings.revalidate_interval = Duration::from_secs(60);
    let Some(rt) = Rt::with(settings).await else {
        return;
    };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let receivers = env_usize("LOAD_FANOUT_RECEIVERS", 250);
    let messages = env_usize("LOAD_FANOUT_MESSAGES", 40);
    let (conv, people) = h.managed_group(&c, receivers + 1).await;
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let connect_started = Instant::now();
    let (_, sockets) = connected_receivers(&rt, &c, &conv, &people[1..]).await;
    let connect_s = connect_started.elapsed().as_secs_f64();
    let readers: Vec<_> = sockets
        .into_iter()
        .map(|mut ws| {
            tokio::spawn(async move {
                let mut out = Vec::new();
                let mut last = 0i64;
                while out.len() < messages {
                    let f = ws.next_of("event").await;
                    if f["event"]["data"]["message"].is_null() {
                        continue;
                    }
                    let seq = f["event"]["data"]["message"]["sequence"].as_i64().unwrap();
                    assert!(seq > last, "in order, no duplicates");
                    last = seq;
                    out.push((seq, Instant::now()));
                }
                out
            })
        })
        .collect();
    let started = Instant::now();
    let mut sent_at = Vec::new();
    for i in 0..messages {
        sent_at.push(Instant::now());
        let r = h
            .send_managed(&c, &conv, people[0], &format!("large fanout {i}"))
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let all: Vec<Vec<(i64, Instant)>> = futures_util::future::join_all(
        readers.into_iter().map(|t| async move { t.await.unwrap() }),
    )
    .await;
    let first_seq = all.iter().map(|v| v[0].0).min().unwrap();
    let mut lat = Vec::new();
    for per in &all {
        for (seq, at) in per {
            lat.push(at.saturating_duration_since(sent_at[(seq - first_seq) as usize]));
        }
    }
    assert_eq!(lat.len(), receivers * messages, "every delivery once");
    report(
        "realtime_large_fanout_latency",
        json!({
            "receivers": receivers, "messages": messages, "deliveries": lat.len(),
            "connect_seconds": connect_s, "seconds": started.elapsed().as_secs_f64(),
            "deliveries_per_second": lat.len() as f64 / started.elapsed().as_secs_f64(),
            "send_to_receive": percentiles(lat),
        }),
    );
}

/// Reconnection storm: every client drops at once, messages are committed while they are
/// away, then ALL reconnect concurrently (new client token + ticket + WebSocket +
/// subscribe-with-cursor) and must replay exactly what they missed.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn realtime_reconnection_storm() {
    let mut settings = test_settings();
    settings.revalidate_interval = Duration::from_secs(60);
    let Some(rt) = Rt::with(settings).await else {
        return;
    };
    let h = &rt.h;
    let c = h.customer(ALL_SCOPES);
    let clients = env_usize("LOAD_STORM_CLIENTS", 200);
    let missed = env_usize("LOAD_STORM_MISSED", 20);
    let (conv, people) = h.managed_group(&c, clients + 1).await;
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let (members, sockets) = connected_receivers(&rt, &c, &conv, &people[1..]).await;
    // One message everyone sees; its event sequence is every client's cursor.
    let r = h
        .send_managed(&c, &conv, people[0], "before the storm")
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let mut cursors = Vec::new();
    for mut ws in sockets {
        let ev = loop {
            let f = ws.next_of("event").await;
            if !f["event"]["data"]["message"].is_null() {
                break f["event"].clone();
            }
        };
        cursors.push(ev["eventSequence"].as_i64().unwrap());
        drop(ws); // the connection is gone (abrupt)
    }
    for i in 0..missed {
        let r = h
            .send_managed(&c, &conv, people[0], &format!("while away {i}"))
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let rt = &rt;
    let c = &c;
    let conv = &conv;
    let started = Instant::now();
    let storm = members.iter().zip(cursors).map(|(m, cursor)| async move {
        let t = Instant::now();
        let mut ws = rt.connect(c, m).await;
        let connected = t.elapsed();
        let sub = ws.subscribe(conv, Some(cursor)).await;
        assert_eq!(sub["type"], "subscribed", "{sub}");
        let mut got = Vec::new();
        while got.len() < missed {
            let f = ws.next_of("event").await;
            if f["event"]["data"]["message"].is_null() {
                continue;
            }
            got.push(f["event"]["data"]["message"]["sequence"].as_i64().unwrap());
        }
        assert!(
            got.windows(2).all(|w| w[0] < w[1]),
            "in order, no duplicates"
        );
        ws.quiet("event", Duration::from_millis(200)).await;
        (connected, t.elapsed())
    });
    let results = futures_util::future::join_all(storm).await;
    let total = started.elapsed();
    let (connect, caught_up): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    report(
        "realtime_reconnection_storm",
        json!({
            "clients": clients, "missed_messages_each": missed, "seconds": total.as_secs_f64(),
            "reconnect": percentiles(connect), "reconnect_to_caught_up": percentiles(caught_up),
        }),
    );
}

struct CleanScanner;

#[async_trait::async_trait]
impl messaging::attachments::scanner::AttachmentScanner for CleanScanner {
    fn engine(&self) -> &'static str {
        "load-test-clean"
    }
    async fn scan(&self, _bytes: &[u8]) -> messaging::attachments::scanner::ScanOutcome {
        messaging::attachments::scanner::ScanOutcome::Clean
    }
}

/// MANAGED attachment pipeline under concurrency: intent → signed upload → finalize
/// (size + SHA-256 verification, content inspection, scan) → sent in a message →
/// download grant for another member. The object
/// store is the in-memory implementation and the scanner always answers CLEAN: the
/// numbers measure Messaging's own work, not a storage backend or a scan engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn attachment_pipeline_throughput() {
    use cf_object_storage::{sha256_hex, MemoryObjectStore, SignedWrite};
    use std::sync::Arc;
    let store = Arc::new(MemoryObjectStore::new());
    let s2 = store.clone();
    let Some(h) = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = Arc::new(CleanScanner);
    })
    .await
    else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let uploads = env_usize("LOAD_ATTACHMENTS", 200);
    let size = env_usize("LOAD_ATTACHMENT_BYTES", 256 * 1024);
    let concurrency = env_usize("LOAD_ATTACHMENT_CONCURRENCY", 16);
    let (conv, people) = h.managed_group(&c, 2).await;
    h.set_policy(&c, json!({
        "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "attachments": { "enabled": true, "maxConcurrentUploads": 1000 },
        "abuse": { "messagesPerMinute": 100000, "uploadsPerHour": 100000 } })).await;
    let id = conv["conversationId"].as_str().unwrap().to_string();
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
    let one = |i: usize| {
        let (h, c, store, id) = (&h, &c, &store, &id);
        let uploader = people[0];
        let reader = people[1];
        async move {
            let mut body = png.to_vec();
            body.extend((0..size).map(|j| ((i * 31 + j) % 251) as u8));
            let t = Instant::now();
            let r = h
                .post(c, Some(uploader), &format!("/v1/conversations/{id}/attachments"),
                    json!({ "kind": "IMAGE", "size": body.len(), "sha256": sha256_hex(&body), "mediaType": "image/png", "fileName": format!("{i}.png") }))
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            let intent = t.elapsed();
            let up = &r.json["upload"];
            let write = SignedWrite {
                method: "PUT",
                url: up["url"].as_str().unwrap().to_string(),
                headers: up["headers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| {
                        (
                            x["name"].as_str().unwrap().to_string(),
                            x["value"].as_str().unwrap().to_string(),
                        )
                    })
                    .collect(),
                expires_at: chrono::DateTime::parse_from_rfc3339(up["expiresAt"].as_str().unwrap())
                    .unwrap()
                    .into(),
            };
            store.upload_signed(&write, h.now(), body).unwrap();
            let aid = r.json["attachmentId"].as_str().unwrap().to_string();
            let t2 = Instant::now();
            let f = h
                .post(
                    c,
                    Some(uploader),
                    &format!("/v1/conversations/{id}/attachments/{aid}/finalize"),
                    json!({}),
                )
                .await;
            assert_eq!(f.json["state"], "READY", "{}", f.text);
            let finalize = t2.elapsed();
            // Sent in a message, then fetched by another member.
            let m = h
                .post(c, Some(uploader), &format!("/v1/conversations/{id}/messages"),
                    json!({ "clientMessageId": uuid::Uuid::new_v4(), "text": "file", "attachmentIds": [aid] }))
                .await;
            assert_eq!(m.status, StatusCode::CREATED, "{}", m.text);
            let g = h
                .post(
                    c,
                    Some(reader),
                    &format!("/v1/conversations/{id}/attachments/{aid}/download-grants"),
                    json!({}),
                )
                .await;
            assert_eq!(g.status, StatusCode::OK, "{}", g.text);
            (intent, finalize, t.elapsed())
        }
    };
    let started = Instant::now();
    let results: Vec<_> = futures_util::stream::iter((0..uploads).map(one))
        .buffer_unordered(concurrency)
        .collect()
        .await;
    let elapsed = started.elapsed();
    let mut intents = Vec::new();
    let mut finals = Vec::new();
    let mut totals = Vec::new();
    for (a, b, t) in results {
        intents.push(a);
        finals.push(b);
        totals.push(t);
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_attachments WHERE state = 'READY'")
            .await,
        uploads as i64
    );
    report(
        "attachment_pipeline_throughput",
        json!({
            "uploads": uploads, "bytes_each": size, "concurrency": concurrency,
            "seconds": elapsed.as_secs_f64(), "uploads_per_second": uploads as f64 / elapsed.as_secs_f64(),
            "verified_mib_per_second": (uploads * size) as f64 / 1048576.0 / elapsed.as_secs_f64(),
            "intent": percentiles(intents), "finalize_verify_scan": percentiles(finals), "end_to_end_with_send_and_grant": percentiles(totals),
        }),
    );
}

/// Outbox / worker throughput: N committed events drained by K concurrent publishers
/// (leases, SKIP LOCKED) into a no-op transport; each event delivered exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore]
async fn outbox_publisher_throughput() {
    use messaging::{
        domain::{RequestContext, SharedClock},
        events::{
            outbox::{
                self, Delivery, EventScope, EventTransport, NewEvent, OutboxPublisher,
                PublisherSettings,
            },
            RetryPolicy,
        },
    };
    use std::sync::{Arc, Mutex};
    struct Count(Mutex<std::collections::HashMap<String, usize>>);
    #[async_trait::async_trait]
    impl EventTransport for Count {
        async fn deliver(&self, envelope: &serde_json::Value) -> Delivery {
            *self
                .0
                .lock()
                .unwrap()
                .entry(envelope["event_id"].as_str().unwrap().to_string())
                .or_default() += 1;
            Delivery::Delivered
        }
    }
    let Some(h) = Harness::new().await else {
        return;
    };
    let events = env_usize("LOAD_OUTBOX_EVENTS", 5000);
    let publishers = env_usize("LOAD_OUTBOX_PUBLISHERS", 4);
    let scope = EventScope {
        tenant_id: uuid::Uuid::new_v4(),
        application_id: uuid::Uuid::new_v4(),
        environment_id: uuid::Uuid::new_v4(),
    };
    let enqueue_started = Instant::now();
    for i in 0..events {
        let device = uuid::Uuid::new_v4();
        outbox::enqueue(
            h.conn(),
            NewEvent {
                event_type: "messaging.device.revoked",
                aggregate_type: "DEVICE",
                aggregate_id: device,
                scope,
                payload: json!({ "device_id": device, "principal_id": uuid::Uuid::new_v4(), "reason": "REPLACED", "transparency_leaf_index": i }),
            },
            &RequestContext::generate(),
            h.now(),
        )
        .await
        .unwrap();
    }
    let enqueue_s = enqueue_started.elapsed().as_secs_f64();
    let transport = Arc::new(Count(Mutex::new(Default::default())));
    let clock: SharedClock = Arc::new(h.clock.clone());
    let started = Instant::now();
    let drains = (0..publishers).map(|_| {
        let p = OutboxPublisher::new(
            h.conn().clone(),
            transport.clone(),
            clock.clone(),
            PublisherSettings {
                lease: chrono::Duration::seconds(30),
                max_attempts: 5,
                retry: RetryPolicy::default(),
                batch: 100,
            },
            "load",
        );
        async move {
            let mut n = 0;
            loop {
                let r = p.publish_due().await.unwrap();
                n += r.published;
                if r.claimed == 0 {
                    return n;
                }
            }
        }
    });
    let per: Vec<usize> = futures_util::future::join_all(drains).await;
    let elapsed = started.elapsed();
    let counts = transport.0.lock().unwrap();
    assert_eq!(per.iter().sum::<usize>(), events);
    assert_eq!(counts.len(), events);
    assert!(
        counts.values().all(|n| *n == 1),
        "each event delivered exactly once"
    );
    report(
        "outbox_publisher_throughput",
        json!({
            "events": events, "publishers": publishers, "enqueue_seconds": enqueue_s,
            "drain_seconds": elapsed.as_secs_f64(), "events_per_second": events as f64 / elapsed.as_secs_f64(),
            "per_publisher": per,
        }),
    );
}
