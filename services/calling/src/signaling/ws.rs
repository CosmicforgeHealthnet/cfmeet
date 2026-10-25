//! The realtime signaling channel `GET /v1/calling/signaling` (Part C §9).
//!
//! * Authenticated BEFORE the upgrade with the endpoint's signaling credential, sent as
//!   `Authorization: Bearer cfsig_…` or — for browsers, which cannot set headers on a
//!   WebSocket — as the subprotocol pair `cfmeet.signaling.v1, bearer.cfsig_…` (the server
//!   answers with `cfmeet.signaling.v1`). Platform API credentials are never accepted.
//! * Delivery only: the channel forwards the endpoint's signals (stamped with the target
//!   endpoint / principal), answers `{"type":"ping"}` with `signaling.pong`, refreshes the
//!   endpoint's presence hint, and closes (4001) when the credential expires. Commands go
//!   through the HTTP API; a lost signal never loses state (recover with
//!   `GET /v1/calling/invitations`).

use axum::{
    extract::{
        ws::{
            rejection::WebSocketUpgradeRejection, CloseFrame, Message, WebSocket, WebSocketUpgrade,
        },
        State,
    },
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    Extension,
};
use chrono::Utc;
use serde_json::{json, Value};

use super::{credentials::TOKEN_PREFIX, signals, SignalingIdentity};
use crate::{
    domain::RequestContext, endpoints::PresenceState, error::CallingError, state::AppState,
};

pub const SUBPROTOCOL: &str = "cfmeet.signaling.v1";
const BEARER_PROTOCOL_PREFIX: &str = "bearer.";
/// Close code: the signaling credential expired; reconnect with a fresh one.
pub const CLOSE_CREDENTIAL_EXPIRED: u16 = 4001;
/// Minimum interval between presence refreshes of one connection.
const PRESENCE_REFRESH_SECONDS: i64 = 30;

/// The signaling token from the Authorization header or the subprotocol list.
fn token(headers: &HeaderMap) -> Option<(String, bool)> {
    if let Some(token) = super::bearer(headers) {
        return Some((token, false));
    }
    let offered = headers
        .get_all("sec-websocket-protocol")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect::<Vec<_>>();
    if !offered.contains(&SUBPROTOCOL) {
        return None;
    }
    offered
        .iter()
        .find_map(|p| p.strip_prefix(BEARER_PROTOCOL_PREFIX))
        .filter(|t| t.starts_with(TOKEN_PREFIX) && t.len() <= 2_048)
        .map(|t| (t.to_string(), true))
}

pub async fn signaling(
    State(state): State<AppState>,
    headers: HeaderMap,
    context: Option<Extension<RequestContext>>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Some((token, via_protocol)) = token(&headers) else {
        return CallingError::AuthenticationFailed.into_response();
    };
    if headers.contains_key("x-cf-service-authorization") {
        return CallingError::AuthenticationFailed.into_response();
    }
    let context = context
        .map(|Extension(c)| c)
        .unwrap_or_else(RequestContext::generate);
    let identity = match state.signaling.authenticate(&token, context).await {
        Ok(identity) => identity,
        Err(error) => return error.into_response(),
    };
    let Ok(upgrade) = upgrade else {
        return CallingError::Validation("A WebSocket upgrade is required.").into_response();
    };
    let upgrade = if via_protocol {
        upgrade.protocols([SUBPROTOCOL])
    } else {
        upgrade
    };
    let mut response = upgrade
        .max_message_size(4 * 1024)
        .on_upgrade(move |socket| serve(state, identity, socket));
    if via_protocol {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
    }
    response
}

async fn serve(state: AppState, identity: SignalingIdentity, mut socket: WebSocket) {
    let endpoint_id = identity.endpoint_id();
    let principal_id = identity.principal_id();
    let mut subscription = state.app.bus().subscribe(endpoint_id.as_uuid());
    let _ = state
        .endpoints
        .record_presence(endpoint_id, PresenceState::Online)
        .await;
    let ready = json!({
        "type": "signaling.ready",
        "endpointId": endpoint_id.as_uuid(),
        "principalId": principal_id.as_uuid(),
        "credentialExpiresAt": identity.expires_at().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    if socket.send(Message::Text(ready.to_string())).await.is_err() {
        return;
    }
    // The service clock is the single time authority (as for authentication).
    let until_expiry = (identity.expires_at() - state.clock.now())
        .to_std()
        .unwrap_or_default();
    let expiry = tokio::time::sleep(until_expiry);
    tokio::pin!(expiry);
    let mut last_presence = Utc::now();
    loop {
        tokio::select! {
            _ = &mut expiry => {
                let _ = socket.send(Message::Close(Some(CloseFrame {
                    code: CLOSE_CREDENTIAL_EXPIRED,
                    reason: "signaling credential expired".into(),
                }))).await;
                break;
            }
            signal = subscription.recv() => {
                let Some(signal) = signal else { break };
                let stamped = signals::for_target(signal, endpoint_id.as_uuid(), principal_id.as_uuid());
                if socket.send(Message::Text(stamped.to_string())).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let parsed: Option<Value> = serde_json::from_str(&text).ok();
                        if parsed.as_ref().and_then(|v| v.get("type")).and_then(Value::as_str) == Some("ping") {
                            let pong = json!({ "type": "signaling.pong", "at": state.clock.now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) });
                            if socket.send(Message::Text(pong.to_string())).await.is_err() {
                                break;
                            }
                        } else {
                            let error = json!({ "type": "signaling.error", "code": "UNSUPPORTED_MESSAGE" });
                            let _ = socket.send(Message::Text(error.to_string())).await;
                        }
                        if (Utc::now() - last_presence).num_seconds() >= PRESENCE_REFRESH_SECONDS {
                            last_presence = Utc::now();
                            let _ = state.endpoints.record_presence(endpoint_id, PresenceState::Online).await;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = socket.send(Message::Pong(payload)).await;
                    }
                    // Complete the closing handshake: reading on flushes the queued close
                    // reply, then the client ends the stream (bounded wait).
                    Some(Ok(Message::Close(_))) => {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                            while let Some(Ok(_)) = socket.recv().await {}
                        })
                        .await;
                        break;
                    }
                    None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    let _ = state
        .endpoints
        .record_presence(endpoint_id, PresenceState::Offline)
        .await;
}
