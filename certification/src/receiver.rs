//! A customer's webhook endpoint: a minimal local HTTP/1.1 server that records every
//! request (path, headers, raw body bytes, arrival time) and answers each path from a
//! script (a queue of replies, then a default reply — `200` unless set otherwise).
//!
//! Signatures are verified with the reference receiver verifier of `packages/webhooks`
//! (`cf_webhooks::signing::verify`: HMAC-SHA256 over the raw body, constant-time compare,
//! timestamp tolerance).

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use cf_webhooks::signing::{
    HEADER_DELIVERY_ID, HEADER_EVENT_ID, HEADER_SIGNATURE, HEADER_TIMESTAMP,
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

/// Largest request body the receiver accepts.
const MAX_BODY: usize = 1024 * 1024;

/// One scripted answer.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn status(status: u16) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// One received request.
#[derive(Debug, Clone)]
pub struct Received {
    pub path: String,
    /// Lower-cased header names.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub at: Instant,
    pub at_utc: chrono::DateTime<chrono::Utc>,
    /// The status the receiver answered.
    pub answered: u16,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn delivery_id(&self) -> &str {
        self.header(HEADER_DELIVERY_ID).unwrap_or("")
    }

    pub fn event_id(&self) -> &str {
        self.header(HEADER_EVENT_ID).unwrap_or("")
    }

    /// The public event type (`type` of the body).
    pub fn event_type(&self) -> String {
        self.json()["type"].as_str().unwrap_or("").to_string()
    }

    /// Verifies the `CF-Webhook-*` headers against `secret` exactly as a customer does.
    pub fn verify(&self, secret: &str) -> anyhow::Result<()> {
        let (Some(sig), Some(ts), Some(id)) = (
            self.header(HEADER_SIGNATURE),
            self.header(HEADER_TIMESTAMP),
            self.header(HEADER_DELIVERY_ID),
        ) else {
            bail!("CF-Webhook-* headers missing: {:?}", self.headers);
        };
        if self.event_id().is_empty() {
            bail!("CF-Webhook-Event-Id missing");
        }
        cf_webhooks::signing::verify(
            secret.as_bytes(),
            sig,
            ts,
            id,
            &self.body,
            chrono::Utc::now().timestamp(),
            cf_webhooks::signing::DEFAULT_TOLERANCE_SECONDS,
        )
        .map_err(|e| anyhow::anyhow!("signature of delivery {id}: {e}"))
    }
}

#[derive(Default)]
struct State {
    scripts: HashMap<String, VecDeque<Reply>>,
    defaults: HashMap<String, Reply>,
    received: Vec<Received>,
}

/// A running receiver (stopped on drop).
pub struct Receiver {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Receiver {
    /// Listens on a free loopback port.
    pub async fn start() -> anyhow::Result<Receiver> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let state = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, state).await;
                });
            }
        });
        Ok(Receiver { addr, state, task })
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Queues `replies` for `path` (answered in order, then the default).
    pub fn script(&self, path: &str, replies: Vec<Reply>) {
        self.state
            .lock()
            .unwrap()
            .scripts
            .entry(path.to_string())
            .or_default()
            .extend(replies);
    }

    /// The answer of `path` once its script is exhausted.
    pub fn set_default(&self, path: &str, reply: Reply) {
        self.state
            .lock()
            .unwrap()
            .defaults
            .insert(path.to_string(), reply);
    }

    /// Everything received on `path`, in arrival order.
    pub fn received(&self, path: &str) -> Vec<Received> {
        self.state
            .lock()
            .unwrap()
            .received
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect()
    }

    /// Everything received on any path.
    pub fn all(&self) -> Vec<Received> {
        self.state.lock().unwrap().received.clone()
    }

    /// Waits until `path` received at least `n` requests (or fails after `timeout`).
    pub async fn wait_for(
        &self,
        path: &str,
        n: usize,
        timeout: Duration,
    ) -> anyhow::Result<Vec<Received>> {
        let deadline = Instant::now() + timeout;
        loop {
            let got = self.received(path);
            if got.len() >= n {
                return Ok(got);
            }
            if Instant::now() > deadline {
                bail!(
                    "{path}: {} of {n} expected webhook requests within {timeout:?}: {:?}",
                    got.len(),
                    got.iter()
                        .map(|r| (r.event_type(), r.answered))
                        .collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Waits until `pred` holds for the requests of `path`.
    pub async fn wait_until(
        &self,
        path: &str,
        timeout: Duration,
        what: &str,
        pred: impl Fn(&[Received]) -> bool,
    ) -> anyhow::Result<Vec<Received>> {
        let deadline = Instant::now() + timeout;
        loop {
            let got = self.received(path);
            if pred(&got) {
                return Ok(got);
            }
            if Instant::now() > deadline {
                bail!(
                    "{path}: {what} not observed within {timeout:?}; received {:?}",
                    got.iter()
                        .map(|r| (r.event_type(), r.answered))
                        .collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

async fn serve(stream: TcpStream, state: Arc<Mutex<State>>) -> anyhow::Result<()> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line).await?;
    let mut parts = line.split_whitespace();
    let _method = parts.next().context("request line")?;
    let target = parts.next().context("request target")?.to_string();
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).await? == 0 {
            bail!("connection closed in the headers");
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        let mut body = Vec::new();
        loop {
            let mut size = String::new();
            r.read_line(&mut size).await?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)?;
            if n == 0 {
                let mut end = String::new();
                r.read_line(&mut end).await?;
                break;
            }
            if body.len() + n > MAX_BODY {
                bail!("body too large");
            }
            let mut chunk = vec![0u8; n + 2];
            r.read_exact(&mut chunk).await?;
            chunk.truncate(n);
            body.extend_from_slice(&chunk);
        }
        body
    } else {
        let n: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if n > MAX_BODY {
            bail!("body too large");
        }
        let mut body = vec![0u8; n];
        r.read_exact(&mut body).await?;
        body
    };
    let reply = {
        let mut s = state.lock().unwrap();
        let reply = s
            .scripts
            .get_mut(&path)
            .and_then(VecDeque::pop_front)
            .or_else(|| s.defaults.get(&path).cloned())
            .unwrap_or_else(|| Reply::status(200));
        s.received.push(Received {
            path,
            headers,
            body,
            at: Instant::now(),
            at_utc: chrono::Utc::now(),
            answered: reply.status,
        });
        reply
    };
    let mut out = format!(
        "HTTP/1.1 {} Certification\r\ncontent-length: 0\r\nconnection: close\r\n",
        reply.status
    );
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut stream = r.into_inner();
    stream.write_all(out.as_bytes()).await?;
    stream.shutdown().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripts_record_and_verify() {
        let rx = Receiver::start().await.unwrap();
        rx.script("/a", vec![Reply::status(500)]);
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let secret = "whsec_receiver-self-test";
        let body = br#"{"id":"whe_1","type":"message.created"}"#.to_vec();
        let ts = chrono::Utc::now().timestamp();
        let sig = cf_webhooks::signing::sign(secret.as_bytes(), ts, "d1", &body);
        let send = || {
            http.post(rx.url("/a?x=1"))
                .header(HEADER_DELIVERY_ID, "d1")
                .header(HEADER_EVENT_ID, "whe_1")
                .header(HEADER_TIMESTAMP, ts.to_string())
                .header(HEADER_SIGNATURE, sig.clone())
                .body(body.clone())
                .send()
        };
        assert_eq!(send().await.unwrap().status().as_u16(), 500);
        assert_eq!(send().await.unwrap().status().as_u16(), 200);
        let got = rx.wait_for("/a", 2, Duration::from_secs(5)).await.unwrap();
        assert_eq!(got[0].answered, 500);
        assert_eq!(got[1].event_type(), "message.created");
        got[1].verify(secret).unwrap();
        assert!(got[1].verify("whsec_other").is_err());
    }
}
