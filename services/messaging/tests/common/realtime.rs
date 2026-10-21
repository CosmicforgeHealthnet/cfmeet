//! A REAL realtime environment for tests: the router served on a TCP port, the outbox
//! publisher running with the realtime fan-out transport, and real WebSocket clients
//! (tokio-tungstenite) speaking the protocol.
#![allow(dead_code)]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::http::StatusCode;
use futures_util::{SinkExt, StreamExt};
use messaging::{
    events::outbox::{OutboxPublisher, PublisherSettings},
    realtime::{
        broker::LocalBroker, ephemeral::MemoryPresence, fanout::RealtimeTransport,
        push::LocalTestPush, token::ClientTokenKeys, Realtime, RealtimeSettings,
    },
};
use serde_json::{json, Value};
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

use super::{flows::Member, *};

pub fn test_settings() -> RealtimeSettings {
    RealtimeSettings {
        revalidate_interval: Duration::from_millis(300),
        heartbeat_interval: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(30),
        allowed_origins: vec!["https://app.example.test".into()],
        drain: Duration::from_millis(500),
        ..Default::default()
    }
}

pub struct Rt {
    pub h: Harness,
    pub addr: SocketAddr,
    pub broker: Arc<LocalBroker>,
    pub push: Arc<LocalTestPush>,
    pub realtime: Arc<Realtime>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Rt {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Rt {
    pub async fn start() -> Option<Self> {
        Self::with(test_settings()).await
    }

    pub async fn with(settings: RealtimeSettings) -> Option<Self> {
        let broker = Arc::new(LocalBroker::default());
        Self::with_backends(
            settings,
            broker.clone(),
            broker,
            Arc::new(MemoryPresence::default()),
        )
        .await
    }

    /// An instance with the given broker / presence backends (`local` is kept for
    /// failure injection; it is the broker unless another one is given).
    pub async fn with_backends(
        settings: RealtimeSettings,
        local: Arc<LocalBroker>,
        broker: Arc<dyn messaging::realtime::broker::Broker>,
        presence: Arc<dyn messaging::realtime::ephemeral::PresenceStore>,
    ) -> Option<Self> {
        Self::assemble(settings, local, broker, presence, None).await
    }

    /// An instance whose push jobs go to `provider` (e.g. the live adapters aimed at
    /// local fakes) instead of the local test recorder.
    pub async fn with_push(
        settings: RealtimeSettings,
        provider: Arc<dyn messaging::realtime::push::PushProvider>,
    ) -> Option<Self> {
        let broker = Arc::new(LocalBroker::default());
        Self::assemble(
            settings,
            broker.clone(),
            broker,
            Arc::new(MemoryPresence::default()),
            Some(provider),
        )
        .await
    }

    async fn assemble(
        settings: RealtimeSettings,
        local: Arc<LocalBroker>,
        broker: Arc<dyn messaging::realtime::broker::Broker>,
        presence: Arc<dyn messaging::realtime::ephemeral::PresenceStore>,
        provider: Option<Arc<dyn messaging::realtime::push::PushProvider>>,
    ) -> Option<Self> {
        let push = Arc::new(LocalTestPush::default());
        let keys = ClientTokenKeys::generate();
        let realtime = Arc::new(Realtime::new(
            settings,
            Some(keys),
            broker,
            presence,
            provider.unwrap_or_else(|| push.clone()),
        ));
        let broker = local;
        let rt2 = realtime.clone();
        let h = Harness::build(move |s| {
            s.realtime = rt2;
        })
        .await?;
        realtime.start();
        let publisher = Arc::new(OutboxPublisher::new(
            h.state.db.clone(),
            Arc::new(RealtimeTransport::new(h.state.clone())),
            h.state.clock.clone(),
            PublisherSettings {
                lease: chrono::Duration::seconds(30),
                max_attempts: 50,
                retry: messaging::events::RetryPolicy {
                    base: chrono::Duration::milliseconds(50),
                    max: chrono::Duration::milliseconds(200),
                },
                batch: 500,
            },
            "test",
        ));
        let mut tasks = vec![tokio::spawn(
            publisher.run_with_wakeup(Duration::from_millis(50), realtime.outbox_wakeup.clone()),
        )];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = h.router();
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        }));
        Some(Self {
            h,
            addr,
            broker,
            push,
            realtime,
            tasks,
        })
    }

    /// A SECOND gateway instance of the same deployment (same database, same token key
    /// via `keys_pem`), with its own hub and the given backends; serves on its own port.
    pub async fn second_instance(
        &mut self,
        broker: Arc<dyn messaging::realtime::broker::Broker>,
        presence: Arc<dyn messaging::realtime::ephemeral::PresenceStore>,
    ) -> (SocketAddr, Arc<Realtime>) {
        let realtime = Arc::new(Realtime::new(
            test_settings(),
            None, // tickets are minted by the first instance and consumed from the DB
            broker,
            presence,
            Arc::new(LocalTestPush::default()),
        ));
        realtime.start();
        let mut state = self.h.state.clone();
        state.realtime = realtime.clone();
        let publisher = Arc::new(OutboxPublisher::new(
            state.db.clone(),
            Arc::new(RealtimeTransport::new(state.clone())),
            state.clock.clone(),
            PublisherSettings {
                lease: chrono::Duration::seconds(30),
                max_attempts: 50,
                retry: messaging::events::RetryPolicy::default(),
                batch: 500,
            },
            "test",
        ));
        self.tasks.push(tokio::spawn(publisher.run_with_wakeup(
            Duration::from_millis(50),
            realtime.outbox_wakeup.clone(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = messaging::http::app(state);
        self.tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        }));
        (addr, realtime)
    }

    /// Opens a WebSocket on another instance's address.
    pub async fn open_at(&self, addr: SocketAddr, ticket: &str) -> Result<Ws, u16> {
        open_ws(addr, ticket, None).await
    }

    /// A client access token for `member`'s device.
    pub async fn token(&self, c: &Customer, m: &Member) -> String {
        let r = self
            .h
            .post(
                c,
                Some(m.principal),
                "/v1/messaging/client-tokens",
                json!({ "deviceId": m.device_id() }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["token"].as_str().unwrap().to_string()
    }

    pub async fn ticket_with(&self, token: &str) -> super::Response {
        self.h
            .send(
                axum::http::Method::POST,
                None,
                None,
                "/v1/realtime/tickets",
                None,
                &[("authorization", &format!("CF-Client-Token {token}"))],
            )
            .await
    }

    pub async fn ticket(&self, c: &Customer, m: &Member) -> String {
        let token = self.token(c, m).await;
        let r = self.ticket_with(&token).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["ticket"].as_str().unwrap().to_string()
    }

    /// Opens a WebSocket with `ticket`; `Err(status)` when the handshake is refused.
    pub async fn open(&self, ticket: &str, origin: Option<&str>) -> Result<Ws, u16> {
        open_ws(self.addr, ticket, origin).await
    }

    pub async fn connect(&self, c: &Customer, m: &Member) -> Ws {
        let t = self.ticket(c, m).await;
        self.open(&t, None).await.expect("connected")
    }
}

pub async fn open_ws(addr: SocketAddr, ticket: &str, origin: Option<&str>) -> Result<Ws, u16> {
    let mut req = format!("ws://{addr}/v1/realtime")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!("cfmeet.messaging.v1, cfmeet.ticket.{ticket}")).unwrap(),
    );
    if let Some(o) = origin {
        req.headers_mut()
            .insert("origin", HeaderValue::from_str(o).unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((stream, resp)) => {
            assert_eq!(
                resp.headers()
                    .get("sec-websocket-protocol")
                    .and_then(|v| v.to_str().ok()),
                Some("cfmeet.messaging.v1"),
                "the ticket is never echoed"
            );
            let mut ws = Ws {
                stream,
                seen: Vec::new(),
            };
            let welcome = ws.next_of("welcome").await;
            assert_eq!(welcome["protocolVersion"], 1);
            Ok(ws)
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("unexpected handshake failure: {e}"),
    }
}

pub struct Ws {
    pub stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// Frames received while waiting for something else.
    pub seen: Vec<Value>,
}

/// What a client observes.
#[derive(Debug)]
pub enum Observed {
    Frame(Value),
    Closed(Option<u16>, String),
    Timeout,
}

impl Ws {
    pub async fn send(&mut self, v: Value) {
        self.stream
            .send(Message::Text(v.to_string()))
            .await
            .unwrap();
    }

    pub async fn recv(&mut self, wait: Duration) -> Observed {
        // One deadline for the whole call: control frames (the server's pings) must not
        // restart the wait, or a frame that never comes hangs the test instead of failing.
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Err(_) => return Observed::Timeout,
                Ok(None) => return Observed::Closed(None, String::new()),
                Ok(Some(Err(_))) => return Observed::Closed(None, String::new()),
                Ok(Some(Ok(Message::Text(t)))) => {
                    let frame: Value = serde_json::from_str(&t).unwrap();
                    // Every server frame conforms to the published protocol contract.
                    conforms(
                        "messaging/realtime/v1.schema.json#/$defs/ServerFrame",
                        &frame,
                    );
                    return Observed::Frame(frame);
                }
                Ok(Some(Ok(Message::Close(f)))) => {
                    return Observed::Closed(
                        f.as_ref().map(|f| u16::from(f.code)),
                        f.map(|f| f.reason.to_string()).unwrap_or_default(),
                    )
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// The next frame of `kind` (others are kept in `seen`).
    pub async fn next_of(&mut self, kind: &str) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.recv(left).await {
                Observed::Frame(f) if f["type"] == kind => return f,
                Observed::Frame(f) => self.seen.push(f),
                other => panic!("waiting for {kind}: {other:?}; seen {:?}", self.seen),
            }
        }
    }

    /// Collects `n` durable events of a conversation.
    pub async fn events(&mut self, n: usize) -> Vec<Value> {
        let mut out = Vec::new();
        while out.len() < n {
            let f = self.next_of("event").await;
            out.push(f["event"].clone());
        }
        out
    }

    /// Asserts nothing of `kind` arrives within `wait`.
    pub async fn quiet(&mut self, kind: &str, wait: Duration) {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return;
            }
            match self.recv(left).await {
                Observed::Frame(f) if f["type"] == kind => panic!("unexpected {kind}: {f}"),
                Observed::Frame(f) => self.seen.push(f),
                Observed::Timeout => return,
                Observed::Closed(c, r) => panic!("closed while quiet: {c:?} {r}"),
            }
        }
    }

    /// Waits for the server to close the connection; returns (code, reason).
    pub async fn closed(&mut self) -> (Option<u16>, String) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.recv(left).await {
                Observed::Closed(c, r) => return (c, r),
                Observed::Frame(f) => self.seen.push(f),
                Observed::Timeout => panic!("not closed; seen {:?}", self.seen),
            }
        }
    }

    pub async fn subscribe(&mut self, conversation: &Value, after: Option<i64>) -> Value {
        let mut f = json!({ "type": "subscribe", "conversationId": conversation["conversationId"], "requestId": "s1" });
        if let Some(a) = after {
            f["afterEventSequence"] = json!(a);
        }
        self.send(f).await;
        loop {
            let deadline = Duration::from_secs(10);
            match self.recv(deadline).await {
                Observed::Frame(f) if f["type"] == "subscribed" || f["type"] == "error" => {
                    return f
                }
                Observed::Frame(f) => self.seen.push(f),
                other => panic!("subscribe: {other:?}"),
            }
        }
    }
}

pub fn cid(conv: &Value) -> Uuid {
    uuid(&conv["conversationId"])
}
