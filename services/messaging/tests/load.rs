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
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let (conv, people) = h.managed_group(&c, 20).await;
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
    let Some(h) = Harness::new().await else {
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
    let got = h.receive(&c, &conv, &mut bob).await;
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
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let receivers = 50;
    let (conv, people) = h.managed_group(&c, receivers + 1).await;
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
    let started = Instant::now();
    let mut sent_at = Vec::new();
    for i in 0..messages {
        sent_at.push(Instant::now());
        let r = h
            .send_managed(&c, &conv, people[0], &format!("fanout {i}"))
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    // Every receiver gets every message once, in order; latency = commit request start →
    // receipt on the socket.
    let mut lat = Vec::new();
    let futures = sockets.into_iter().map(|mut ws| async move {
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
    });
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
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "abuse": { "messagesPerMinute": 100000 } })).await;
    let (conv, people) = h.managed_group(&c, 2).await;
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
            assert_eq!(
                h.send_managed(c, conv, p, &text).await.status,
                StatusCode::CREATED
            )
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
