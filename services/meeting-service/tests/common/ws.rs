//! WebSocket helpers for multi-actor E2E (realtime ticket → stream → frames).
#![allow(dead_code)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use super::{Auth, TestApp};

pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Ticket (authorized) + upgrade. `cursor` resumes after a sequence.
pub async fn subscribe(
    app: &TestApp,
    addr: std::net::SocketAddr,
    cid: Uuid,
    auth: Auth<'_>,
    cursor: Option<i64>,
) -> Ws {
    let (s, t) = app
        .post(
            format!("/api/v2/conferences/{cid}/realtime/ticket"),
            auth,
            json!({}),
        )
        .await;
    assert!(s.is_success(), "ticket: {s} {t}");
    let mut url = format!(
        "ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={}",
        t["ticket"].as_str().unwrap()
    );
    if let Some(c) = cursor {
        url.push_str(&format!("&cursor={c}"));
    }
    tokio_tungstenite::connect_async(url)
        .await
        .expect("ws upgrade")
        .0
}

/// Next JSON frame, or `None` on timeout / close.
pub async fn frame(ws: &mut Ws, within: Duration) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(Message::Ping(p)))) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) | Err(_) => return None,
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Wait for an event frame of `event_type`; returns the event envelope.
pub async fn expect_event(ws: &mut Ws, event_type: &str, within: Duration) -> Value {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen = vec![];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match frame(ws, left).await {
            Some(f) if f["type"] == "event" && f["event"]["eventType"] == event_type => {
                return f["event"].clone()
            }
            Some(f) => seen.push(f),
            None => panic!("no `{event_type}` event within {within:?}; frames: {seen:?}"),
        }
    }
}

/// Collect every frame until `quiet` passes without one.
pub async fn drain(ws: &mut Ws, quiet: Duration) -> Vec<Value> {
    let mut out = vec![];
    while let Some(f) = frame(ws, quiet).await {
        out.push(f);
    }
    out
}
