//! LiveKit side of Calling's media connection gateway (ADR 0007).
//!
//! Self-hosted LiveKit cannot revoke the tokens it issues or refreshes, and a privileged
//! refreshed token (sent to a client after `UpdateParticipant`) would let a removed
//! session rejoin with publish / subscribe rights. The gateway closes that gap:
//!
//! * the client presents its edge-sealed credential (`Authorization: Bearer` or
//!   `access_token`); a LiveKit-signed token is refused;
//! * Calling's authoritative admission decides from the database — current generation,
//!   AUTHORIZED, session / participant / Call eligible — and refuses (HTTP 401, 503 when
//!   the state is unreadable) BEFORE LiveKit is contacted;
//! * an admitted connection is relayed to LiveKit with a fresh 30 s join-only token for
//!   exactly that room / identity / generation (join-then-admit is unchanged: media rights
//!   are applied server-side after the join);
//! * downstream, every `refresh_token` LiveKit sends is re-checked against the database
//!   and re-sealed with the edge key (or the connection is closed); `room_moved` (which
//!   carries a token) and non-protobuf frames close the connection; upstream, simulation
//!   requests are dropped and only the SDK's documented query parameters are forwarded.
//!
//! The client therefore never holds any token LiveKit itself accepts.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    extract::ws::{CloseFrame, Message as DownMessage, WebSocket},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest,
        protocol::{frame::coding::CloseCode, CloseFrame as UpCloseFrame, WebSocketConfig},
        Message as UpMessage,
    },
    MaybeTlsStream, WebSocketStream,
};

use super::{
    gateway::{
        ConnectionAdmission, ConnectionClaims, ConnectionDenied, GatewayPath, GatewayRequest,
        MediaConnectionGateway,
    },
    livekit::LiveKitAdapter,
};

/// Largest signaling frame relayed in either direction.
const MAX_FRAME_BYTES: usize = 1 << 20;
/// Upstream connect timeout.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Query parameters of the LiveKit client SDKs that are forwarded upstream.
const FORWARDED_QUERY: &[&str] = &[
    "sdk",
    "version",
    "protocol",
    "client_protocol",
    "os",
    "os_version",
    "device_model",
    "browser",
    "browser_version",
    "network",
    "auto_subscribe",
    "adaptive_stream",
    "reconnect",
    "reconnect_reason",
    "sid",
    "join_request",
];

/// `SignalResponse` fields the gateway acts on (livekit_rtc.proto).
const RESPONSE_JOIN: u64 = 1;
const RESPONSE_REFRESH_TOKEN: u64 = 16;
const RESPONSE_RECONNECT: u64 = 19;
const RESPONSE_ROOM_MOVED: u64 = 24;
/// `SignalRequest.simulate` (test-only scenarios): never relayed.
const REQUEST_SIMULATE: u64 = 13;

/// The gateway in front of one LiveKit deployment.
pub struct LiveKitGateway {
    adapter: Arc<LiveKitAdapter>,
}

impl LiveKitGateway {
    pub fn new(adapter: Arc<LiveKitAdapter>) -> Self {
        Self { adapter }
    }
}

fn refused(status: StatusCode, reason: &'static str) -> Response {
    let mut response = (status, reason).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn deny(denied: ConnectionDenied) -> Response {
    match denied {
        ConnectionDenied::Unavailable => refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "media admission unavailable",
        ),
        _ => refused(StatusCode::UNAUTHORIZED, "media connection not admitted"),
    }
}

/// The presented credential: exactly one of `Authorization: Bearer` or `access_token`.
fn presented_token(headers: &HeaderMap, query: &[(String, String)]) -> Option<String> {
    let mut from_header = headers.get_all(header::AUTHORIZATION).iter();
    let header_token = match (from_header.next(), from_header.next()) {
        (Some(v), None) => {
            let (scheme, token) = v.to_str().ok()?.split_once(' ')?;
            if !scheme.eq_ignore_ascii_case("bearer") {
                return None;
            }
            Some(token.trim().to_string())
        }
        (None, _) => None,
        _ => return None,
    };
    let query_tokens: Vec<&String> = query
        .iter()
        .filter(|(k, _)| k == "access_token")
        .map(|(_, v)| v)
        .collect();
    match (header_token, query_tokens.as_slice()) {
        (Some(t), []) => Some(t),
        (None, [t]) => Some((*t).clone()),
        _ => None,
    }
}

fn parse_query(raw: Option<&str>) -> Option<Vec<(String, String)>> {
    let raw = raw.unwrap_or_default();
    if raw.len() > 16 * 1024 {
        return None;
    }
    Some(
        url_decode_pairs(raw)
            .into_iter()
            .filter(|(k, _)| !k.is_empty())
            .collect(),
    )
}

/// `application/x-www-form-urlencoded` pairs (no external dependency).
fn url_decode_pairs(raw: &str) -> Vec<(String, String)> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    let hex = |b: u8| (b as char).to_digit(16);
                    match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                        (Some(h), Some(l)) => {
                            out.push((h * 16 + l) as u8);
                            i += 2;
                        }
                        _ => out.push(b'%'),
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The upstream query: the SDK's documented parameters only (never a token).
fn forwarded_query(query: &[(String, String)]) -> String {
    query
        .iter()
        .filter(|(k, _)| FORWARDED_QUERY.contains(&k.as_str()))
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn write_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// The single top-level field of a `SignalRequest` / `SignalResponse` frame (both are a
/// `oneof`): (field number, length-delimited payload if any). `None`: not a well-formed
/// single-field message.
fn signal_field(frame: &[u8]) -> Option<(u64, Option<&[u8]>)> {
    let mut pos = 0;
    let key = read_varint(frame, &mut pos)?;
    let (field, wire) = (key >> 3, key & 7);
    if field == 0 {
        return None;
    }
    let payload = match wire {
        0 => {
            read_varint(frame, &mut pos)?;
            None
        }
        1 => {
            pos = pos.checked_add(8)?;
            None
        }
        2 => {
            let len = usize::try_from(read_varint(frame, &mut pos)?).ok()?;
            let end = pos.checked_add(len)?;
            let payload = frame.get(pos..end)?;
            pos = end;
            Some(payload)
        }
        5 => {
            pos = pos.checked_add(4)?;
            None
        }
        _ => return None,
    };
    (pos == frame.len()).then_some((field, payload))
}

fn refresh_frame(token: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(token.len() + 4);
    write_varint((RESPONSE_REFRESH_TOKEN << 3) | 2, &mut out);
    write_varint(token.len() as u64, &mut out);
    out.extend_from_slice(token.as_bytes());
    out
}

/// What to do with one frame LiveKit sends to the client.
#[derive(Debug, PartialEq, Eq)]
enum Downstream {
    Forward,
    /// The connection is (re)established: apply server-side permissions.
    Joined,
    /// A refreshed LiveKit token to re-seal.
    Refresh(String),
    Close,
}

fn classify_downstream(frame: &[u8]) -> Downstream {
    match signal_field(frame) {
        Some((RESPONSE_REFRESH_TOKEN, Some(token))) => match std::str::from_utf8(token) {
            Ok(token) => Downstream::Refresh(token.to_string()),
            Err(_) => Downstream::Close,
        },
        Some((RESPONSE_ROOM_MOVED, _)) | None => Downstream::Close,
        Some((RESPONSE_JOIN | RESPONSE_RECONNECT, _)) => Downstream::Joined,
        Some(_) => Downstream::Forward,
    }
}

/// Whether a client frame may be relayed to LiveKit (`None`: close the connection).
fn upstream_allowed(frame: &[u8]) -> Option<bool> {
    let (field, _) = signal_field(frame)?;
    Some(field != REQUEST_SIMULATE)
}

type Upstream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[async_trait]
impl MediaConnectionGateway for LiveKitGateway {
    async fn handle(
        &self,
        request: GatewayRequest,
        admission: Arc<dyn ConnectionAdmission>,
    ) -> Response {
        let Some(query) = parse_query(request.query.as_deref()) else {
            return refused(StatusCode::BAD_REQUEST, "malformed query");
        };
        let Some(token) = presented_token(&request.headers, &query) else {
            return deny(ConnectionDenied::InvalidCredential);
        };
        let claims = match self.adapter.verify_edge_credential(&token, Utc::now()) {
            Ok(claims) => claims,
            Err(denied) => {
                tracing::info!(
                    reason = denied.as_str(),
                    "media connection refused (credential)"
                );
                return deny(denied);
            }
        };
        if let Err(denied) = admission.admit_connection(&claims).await {
            return deny(denied);
        }
        if request.validate_only {
            return (StatusCode::OK, "success").into_response();
        }
        let Ok(upgrade) = request.upgrade else {
            return refused(StatusCode::BAD_REQUEST, "a WebSocket upgrade is required");
        };
        let upstream = match self.open_upstream(request.path, &claims, &query).await {
            Ok(upstream) => upstream,
            Err(()) => return refused(StatusCode::SERVICE_UNAVAILABLE, "media server unavailable"),
        };
        let adapter = self.adapter.clone();
        upgrade
            .max_message_size(MAX_FRAME_BYTES)
            .on_upgrade(move |socket| relay(socket, upstream, claims, adapter, admission))
    }
}

impl LiveKitGateway {
    async fn open_upstream(
        &self,
        path: GatewayPath,
        claims: &ConnectionClaims,
        query: &[(String, String)],
    ) -> Result<Upstream, ()> {
        let token = self.adapter.upstream_token(claims).map_err(|_| ())?;
        let suffix = match path {
            GatewayPath::Legacy => "/rtc",
            GatewayPath::V1 => "/rtc/v1",
        };
        let forwarded = forwarded_query(query);
        let url = if forwarded.is_empty() {
            format!("{}{suffix}", self.adapter.signal_url())
        } else {
            format!("{}{suffix}?{forwarded}", self.adapter.signal_url())
        };
        let mut upstream_request = url.into_client_request().map_err(|_| ())?;
        upstream_request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| ())?,
        );
        let config = WebSocketConfig {
            max_message_size: Some(MAX_FRAME_BYTES),
            max_frame_size: Some(MAX_FRAME_BYTES),
            ..Default::default()
        };
        match tokio::time::timeout(
            UPSTREAM_CONNECT_TIMEOUT,
            connect_async_with_config(upstream_request, Some(config), false),
        )
        .await
        {
            Ok(Ok((stream, _))) => Ok(stream),
            _ => {
                tracing::warn!("media gateway could not reach the media server");
                Err(())
            }
        }
    }
}

/// Relays one admitted connection until either side closes.
async fn relay(
    downstream: WebSocket,
    upstream: Upstream,
    claims: ConnectionClaims,
    adapter: Arc<LiveKitAdapter>,
    admission: Arc<dyn ConnectionAdmission>,
) {
    let (mut down_tx, mut down_rx) = downstream.split();
    let (mut up_tx, mut up_rx) = upstream.split();
    loop {
        tokio::select! {
            from_client = down_rx.next() => {
                let message = match from_client {
                    Some(Ok(DownMessage::Binary(frame))) => match upstream_allowed(&frame) {
                        Some(true) => UpMessage::Binary(frame),
                        Some(false) => continue,
                        None => break,
                    },
                    Some(Ok(DownMessage::Ping(_) | DownMessage::Pong(_))) => continue,
                    // Text (JSON) signaling is never relayed; close ends the relay.
                    _ => break,
                };
                if up_tx.send(message).await.is_err() {
                    break;
                }
            }
            from_server = up_rx.next() => {
                let frame = match from_server {
                    Some(Ok(UpMessage::Binary(frame))) => frame,
                    Some(Ok(UpMessage::Ping(_) | UpMessage::Pong(_) | UpMessage::Frame(_))) => continue,
                    _ => break,
                };
                let outgoing = match classify_downstream(&frame) {
                    Downstream::Forward => frame,
                    Downstream::Joined => {
                        let admission = admission.clone();
                        let joined = claims.clone();
                        tokio::spawn(async move { admission.connection_established(&joined).await });
                        frame
                    }
                    Downstream::Refresh(token) => {
                        // A refreshed (possibly privileged) token is handed over only while
                        // the session is still admitted, and only re-sealed with the edge key.
                        if admission.admit_connection(&claims).await.is_err() {
                            break;
                        }
                        match adapter.reseal_refreshed(&token, &claims, Utc::now()) {
                            Some(sealed) => refresh_frame(&sealed),
                            None => break,
                        }
                    }
                    Downstream::Close => break,
                };
                if down_tx.send(DownMessage::Binary(outgoing)).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = up_tx
        .send(UpMessage::Close(Some(UpCloseFrame {
            code: CloseCode::Normal,
            reason: "".into(),
        })))
        .await;
    let _ = down_tx
        .send(DownMessage::Close(Some(CloseFrame {
            code: 1000,
            reason: "".into(),
        })))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_frames_are_classified_by_their_single_field() {
        let refresh = refresh_frame("a.b.c");
        assert_eq!(
            classify_downstream(&refresh),
            Downstream::Refresh("a.b.c".into())
        );
        // join (field 1, length-delimited)
        assert_eq!(classify_downstream(&[0x0a, 0x00]), Downstream::Joined);
        // reconnect (field 19)
        let mut reconnect = Vec::new();
        write_varint((19 << 3) | 2, &mut reconnect);
        reconnect.push(0);
        assert_eq!(classify_downstream(&reconnect), Downstream::Joined);
        // room_moved (field 24) carries a token: close.
        let mut moved = Vec::new();
        write_varint((24 << 3) | 2, &mut moved);
        moved.push(0);
        assert_eq!(classify_downstream(&moved), Downstream::Close);
        // update (field 5) passes.
        assert_eq!(
            classify_downstream(&[0x2a, 0x01, 0x00]),
            Downstream::Forward
        );
        // Garbage / truncated / trailing bytes / two fields: close.
        for bad in [
            &[][..],
            &[0xff][..],
            &[0x0a, 0x05, 0x00][..],
            &[0x0a, 0x00, 0x2a, 0x00][..],
        ] {
            assert_eq!(classify_downstream(bad), Downstream::Close, "{bad:?}");
        }
        // Client side: simulate requests are dropped, malformed frames close.
        let mut simulate = Vec::new();
        write_varint((13 << 3) | 2, &mut simulate);
        simulate.push(0);
        assert_eq!(upstream_allowed(&simulate), Some(false));
        assert_eq!(upstream_allowed(&[0x0a, 0x00]), Some(true));
        assert_eq!(upstream_allowed(&[0x0a]), None);
    }

    #[test]
    fn credentials_come_from_exactly_one_place_and_never_travel_upstream() {
        let mut headers = HeaderMap::new();
        let none: Vec<(String, String)> = Vec::new();
        assert_eq!(presented_token(&headers, &none), None);
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t1"));
        assert_eq!(presented_token(&headers, &none).as_deref(), Some("t1"));
        let q = vec![("access_token".to_string(), "t2".to_string())];
        assert_eq!(
            presented_token(&headers, &q),
            None,
            "two credentials are ambiguous"
        );
        assert_eq!(
            presented_token(&HeaderMap::new(), &q).as_deref(),
            Some("t2")
        );
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic t1"));
        assert_eq!(presented_token(&headers, &none), None);

        let query = parse_query(Some(
            "access_token=secret&sdk=python&join_request=a%2Bb&evil=1&reconnect=1",
        ))
        .unwrap();
        let forwarded = forwarded_query(&query);
        assert!(!forwarded.contains("secret") && !forwarded.contains("access_token"));
        assert!(!forwarded.contains("evil"));
        assert!(forwarded.contains("sdk=python") && forwarded.contains("join_request=a%2Bb"));
        assert!(forwarded.contains("reconnect=1"));
    }
}
