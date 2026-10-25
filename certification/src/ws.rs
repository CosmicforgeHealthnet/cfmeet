//! WebSocket clients for the realtime channels the gateway forwards.

use std::time::Duration;

use anyhow::{bail, Context};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::{
        client::IntoClientRequest,
        http::HeaderValue,
        protocol::{frame::coding::CloseCode, CloseFrame},
        Message,
    },
    MaybeTlsStream, WebSocketStream,
};

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// An open WebSocket and the subprotocol the server selected.
pub struct Ws {
    pub socket: Socket,
    pub protocol: Option<String>,
}

/// Opens `url` (`http://` is mapped to `ws://`). `Err(status)` when the upgrade is
/// refused with an HTTP response (relayed by the gateway from the owning service).
pub async fn open(
    url: &str,
    protocols: Option<&str>,
    bearer: Option<&str>,
) -> anyhow::Result<Result<Ws, (u16, String)>> {
    let url = url.replacen("http://", "ws://", 1);
    let mut req = url.as_str().into_client_request()?;
    if let Some(p) = protocols {
        req.headers_mut()
            .insert("sec-websocket-protocol", HeaderValue::from_str(p)?);
    }
    if let Some(t) = bearer {
        req.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {t}"))?,
        );
    }
    match tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(req),
    )
    .await
    .context("WebSocket handshake timed out")?
    {
        Ok((socket, resp)) => Ok(Ok(Ws {
            socket,
            protocol: resp
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        })),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            let status = r.status().as_u16();
            let body = r
                .into_body()
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default();
            Ok(Err((status, body)))
        }
        Err(e) => bail!("WebSocket connection to {url} failed: {e}"),
    }
}

impl Ws {
    /// The next JSON text frame (pings are answered by the library; pongs skipped).
    pub async fn next_json(&mut self, within: Duration) -> anyhow::Result<Value> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let msg = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .context("no frame received in time")?
                .context("the socket closed")??;
            match msg {
                Message::Text(t) => return Ok(serde_json::from_str(&t)?),
                Message::Binary(b) => return Ok(serde_json::from_slice(&b)?),
                Message::Close(c) => bail!("closed by the server: {c:?}"),
                _ => continue,
            }
        }
    }

    /// The next JSON frame whose `type` is `kind` (other frames are skipped).
    pub async fn next_of(&mut self, kind: &str, within: Duration) -> anyhow::Result<Value> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let v = self.next_json(left).await?;
            if v["type"] == kind {
                return Ok(v);
            }
        }
    }

    pub async fn send_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.socket.send(Message::Text(text.to_string())).await?;
        Ok(())
    }

    pub async fn send_json(&mut self, v: &Value) -> anyhow::Result<()> {
        self.socket.send(Message::Text(v.to_string())).await?;
        Ok(())
    }

    /// A WebSocket ping; returns when the matching pong arrives.
    pub async fn ping(&mut self, within: Duration) -> anyhow::Result<()> {
        self.socket
            .send(Message::Ping(b"cert-heartbeat".to_vec()))
            .await?;
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .context("no pong in time")?
            {
                Some(Ok(Message::Pong(p))) if p == b"cert-heartbeat" => return Ok(()),
                Some(Ok(Message::Close(c))) => bail!("closed instead of pong: {c:?}"),
                Some(Ok(_)) => continue,
                Some(Err(e)) => bail!("socket error: {e}"),
                None => bail!("socket ended"),
            }
        }
    }

    /// Clean close (1000) and the server's close frame code.
    pub async fn close(mut self) -> anyhow::Result<Option<u16>> {
        self.socket
            .close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "certification done".into(),
            }))
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, self.socket.next()).await {
                Err(_) => bail!("no close acknowledgement in time"),
                Ok(None) => return Ok(None),
                Ok(Some(Ok(Message::Close(c)))) => return Ok(c.map(|c| u16::from(c.code))),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed))) => {
                    return Ok(None)
                }
                Ok(Some(Err(e))) => bail!("socket error while closing: {e}"),
            }
        }
    }
}
