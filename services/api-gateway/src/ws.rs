//! Realtime WebSocket forwarding (Part VI §7.6) for the approved channels: Meeting
//! realtime (`/api/v2/conferences/{id}/realtime`), Calling signaling
//! (`/v1/calling/signaling`) and Messaging realtime (`/v1/realtime`).
//!
//! The security model of each channel is untouched: tickets / credentials stay
//! short-lived and service-specific and the gateway only TRANSPORTS them (subprotocol,
//! query or `Authorization`), never inspects, stores or logs them. Messaging E2EE frames
//! are relayed opaquely.
//!
//! 1. The upstream handshake runs FIRST, with the client's offered subprotocols, query
//!    and transported credential. A refusal (401 / 403 / 404 / 409 …) is relayed to the
//!    client as an HTTP response — the client is never upgraded to a dead socket.
//! 2. The client is upgraded with the subprotocol the upstream SELECTED.
//! 3. Frames are pumped both ways in order, one at a time (a slow peer back-pressures
//!    the other side instead of buffering without bound), with a per-message size limit.
//!    Ping / pong are relayed (heartbeats stay end-to-end); a close frame's code and
//!    reason are relayed and end both directions.

use axum::{
    extract::{
        ws::{CloseFrame as AxumClose, Message as AxumMessage, WebSocket, WebSocketUpgrade},
        FromRequestParts,
    },
    http::{header, request::Parts, HeaderValue},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{
    self,
    client::IntoClientRequest,
    protocol::{frame::coding::CloseCode, CloseFrame, WebSocketConfig},
    Message as WsMessage,
};

use crate::{
    http::error::{ApiError, ErrorCode},
    http::ingress::{forwarded_headers, upstream_url, valid_query, Presented},
    routing::{AuthMode, RouteMatch},
    state::AppState,
};

fn ws_url(http_url: &str) -> Option<String> {
    if let Some(rest) = http_url.strip_prefix("https://") {
        Some(format!("wss://{rest}"))
    } else {
        http_url
            .strip_prefix("http://")
            .map(|rest| format!("ws://{rest}"))
    }
}

pub async fn proxy(
    state: &AppState,
    route: &RouteMatch<'_>,
    parts: &mut Parts,
    auth: Option<&Presented>,
    mode: AuthMode,
) -> Response {
    let route_id = route.definition.id.clone();
    let query = match valid_query(parts.uri.query()) {
        Ok(q) => q,
        Err(e) => return e.into_response(),
    };
    let Some(url) = upstream_url(state, route, query).and_then(|u| ws_url(&u)) else {
        return ApiError::Code(ErrorCode::ServiceUnavailable).into_response();
    };
    let upgrade = match WebSocketUpgrade::from_request_parts(parts, state).await {
        Ok(u) => u,
        Err(_) => return ApiError::Code(ErrorCode::ValidationError).into_response(),
    };
    let Ok(mut request) = url.as_str().into_client_request() else {
        return ApiError::Code(ErrorCode::ValidationError).into_response();
    };
    for (name, value) in forwarded_headers(parts, route, Some(mode), auth).iter() {
        request.headers_mut().insert(name.clone(), value.clone());
    }
    if let Some(protocols) = parts
        .headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .filter(|v| v.len() <= 4096)
    {
        request
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, protocols.clone());
    }
    let max = state.ws_max_message_bytes;
    let config = WebSocketConfig {
        max_message_size: Some(max),
        max_frame_size: Some(max),
        ..Default::default()
    };
    let connect = tokio_tungstenite::connect_async_with_config(request, Some(config), false);
    let (upstream, handshake) = match tokio::time::timeout(state.upstream_timeout, connect).await {
        Err(_) => return ApiError::Code(ErrorCode::UpstreamTimeout).into_response(),
        Ok(Err(tungstenite::Error::Http(refusal))) => {
            // The owning service refused the upgrade: relay its answer.
            let status = refusal.status();
            tracing::info!(route = %route_id, status = status.as_u16(), "websocket upgrade refused upstream");
            let body = refusal.into_body().unwrap_or_default();
            let mut r = Response::builder()
                .status(status.as_u16())
                .header(header::CACHE_CONTROL, "no-store");
            if !body.is_empty() && serde_json::from_slice::<serde::de::IgnoredAny>(&body).is_ok() {
                r = r.header(header::CONTENT_TYPE, "application/json");
            }
            return r
                .body(axum::body::Body::from(body))
                .unwrap_or_else(|_| ApiError::Code(ErrorCode::InternalError).into_response());
        }
        Ok(Err(e)) => {
            tracing::warn!(route = %route_id, error = %e, "websocket upstream unavailable");
            return ApiError::Code(ErrorCode::UpstreamUnavailable).into_response();
        }
        Ok(Ok(pair)) => pair,
    };
    let selected = handshake
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut upgrade = upgrade
        .max_message_size(max)
        .max_frame_size(max)
        .on_failed_upgrade(|e| tracing::info!(error = %e, "client websocket upgrade failed"));
    if let Some(p) = selected.clone() {
        upgrade = upgrade.protocols([p]);
    }
    tracing::info!(route = %route_id, "websocket established");
    let mut response = upgrade.on_upgrade(move |client| async move {
        pump(client, upstream, route_id).await;
    });
    if let Some(p) = selected.and_then(|p| HeaderValue::from_str(&p).ok()) {
        response
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, p);
    }
    response
}

fn to_upstream(m: AxumMessage) -> WsMessage {
    match m {
        AxumMessage::Text(t) => WsMessage::Text(t),
        AxumMessage::Binary(b) => WsMessage::Binary(b),
        AxumMessage::Ping(p) => WsMessage::Ping(p),
        AxumMessage::Pong(p) => WsMessage::Pong(p),
        AxumMessage::Close(c) => WsMessage::Close(c.map(|c| CloseFrame {
            code: CloseCode::from(c.code),
            reason: c.reason,
        })),
    }
}

fn to_client(m: WsMessage) -> Option<AxumMessage> {
    Some(match m {
        WsMessage::Text(t) => AxumMessage::Text(t),
        WsMessage::Binary(b) => AxumMessage::Binary(b),
        WsMessage::Ping(p) => AxumMessage::Ping(p),
        WsMessage::Pong(p) => AxumMessage::Pong(p),
        WsMessage::Close(c) => AxumMessage::Close(c.map(|c| AxumClose {
            code: c.code.into(),
            reason: c.reason,
        })),
        WsMessage::Frame(_) => return None,
    })
}

/// Time allowed for the other direction to finish its closing handshake.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

async fn pump<S>(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<S>,
    route_id: String,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut client_tx, mut client_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();
    let client_to_upstream = async {
        while let Some(Ok(m)) = client_rx.next().await {
            let closing = matches!(m, AxumMessage::Close(_));
            if up_tx.send(to_upstream(m)).await.is_err() || closing {
                break;
            }
        }
        let _ = up_tx.close().await;
    };
    let upstream_to_client = async {
        let mut relayed_close = false;
        while let Some(Ok(m)) = up_rx.next().await {
            let closing = matches!(m, WsMessage::Close(_));
            if let Some(m) = to_client(m) {
                if client_tx.send(m).await.is_err() {
                    return;
                }
                if closing {
                    relayed_close = true;
                    break;
                }
            }
        }
        if !relayed_close {
            // The owning service went away without a closing handshake.
            let _ = client_tx
                .send(AxumMessage::Close(Some(AxumClose {
                    code: 1011,
                    reason: "upstream closed".into(),
                })))
                .await;
        }
        let _ = client_tx.close().await;
    };
    // When one direction ends, the other gets a short grace period to complete the
    // closing handshake (dropping it at once would reset the connection and lose the
    // close frame in flight).
    tokio::pin!(client_to_upstream, upstream_to_client);
    tokio::select! {
        _ = &mut client_to_upstream => {
            let _ = tokio::time::timeout(CLOSE_GRACE, &mut upstream_to_client).await;
        },
        _ = &mut upstream_to_client => {
            let _ = tokio::time::timeout(CLOSE_GRACE, &mut client_to_upstream).await;
        },
    }
    tracing::info!(route = %route_id, "websocket closed");
}
