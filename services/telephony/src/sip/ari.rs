//! Asterisk ARI adapter of [`super::SipMediaEdge`] (Part E §41; ADR 0015).
//!
//! The self-operated SIP / media edge is Asterisk (B2BUA, PJSIP trunks, mixing bridges
//! that anchor media, DTMF, hold, redirect). Telephony drives it ONLY through the Asterisk
//! REST Interface (ARI): REST calls for control, one authenticated event WebSocket per
//! instance for events. The ARI listener is bound to a private interface; its credential
//! is read from a secret FILE.
//!
//! Correlation: an originated channel's id IS Telephony's correlation key (set with
//! `channelId` before the request), so events, read-back and the provider binding agree
//! even when the HTTP answer is lost. Inbound dialogs enter the control application from
//! the trunk context (`inbound`, peer = PJSIP endpoint) or the internal application-media
//! context (`appmedia`, the dialled extension is the media join code).
//!
//! Failure semantics: a refused TCP connection means nothing was sent (`Unavailable`); a
//! timeout or a broken response after sending means the action may have happened
//! (`Timeout`).

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use tokio::sync::broadcast;
use zeroize::Zeroizing;

use super::{
    ChannelRef, ChannelState, ChannelSummary, EdgeConfigError, EdgeEnterpriseDialog, EdgeError,
    EdgeEvent, EdgeEventKind, EdgeInbound, EdgeOriginate, EdgeResult, SipMediaEdge,
};
use crate::{
    domain::{EndReason, Health, LegStatus},
    providers::ExecutionNumber,
};

/// ARI configuration (environment).
#[derive(Clone)]
pub struct AriSettings {
    /// `http://127.0.0.1:8088` — a private listener; never internet-exposed.
    pub base_url: String,
    pub username: String,
    pub password: Zeroizing<String>,
    /// Stasis application name.
    pub app: String,
    pub request_timeout: Duration,
}

impl std::fmt::Debug for AriSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AriSettings")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

impl AriSettings {
    /// `TELEPHONY_ARI_URL`, `TELEPHONY_ARI_USERNAME`, `TELEPHONY_ARI_PASSWORD_FILE`
    /// (secret file; inline passwords are refused), `TELEPHONY_ARI_APP`
    /// (default `cfmeet-telephony`). Outside `local` / `test` the URL must not be a
    /// public address over plain HTTP… it must be loopback or a private network.
    pub fn from_env(
        environment: cf_service_auth::RuntimeEnvironment,
    ) -> Result<Self, EdgeConfigError> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        if get("TELEPHONY_ARI_PASSWORD").is_some() {
            return Err(EdgeConfigError::Invalid(
                "TELEPHONY_ARI_PASSWORD",
                "inline passwords are refused; use TELEPHONY_ARI_PASSWORD_FILE".into(),
            ));
        }
        let base_url =
            get("TELEPHONY_ARI_URL").ok_or(EdgeConfigError::Missing("TELEPHONY_ARI_URL"))?;
        let username = get("TELEPHONY_ARI_USERNAME")
            .ok_or(EdgeConfigError::Missing("TELEPHONY_ARI_USERNAME"))?;
        let file = get("TELEPHONY_ARI_PASSWORD_FILE")
            .ok_or(EdgeConfigError::Missing("TELEPHONY_ARI_PASSWORD_FILE"))?;
        let password = std::fs::read_to_string(&file).map_err(|_| {
            EdgeConfigError::Invalid("TELEPHONY_ARI_PASSWORD_FILE", "cannot be read".into())
        })?;
        let password = Zeroizing::new(password.trim().to_string());
        if password.len() < 16 {
            return Err(EdgeConfigError::Invalid(
                "TELEPHONY_ARI_PASSWORD_FILE",
                "the password must have at least 16 characters".into(),
            ));
        }
        Self::validate_url(&base_url, environment)?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            username,
            password,
            app: get("TELEPHONY_ARI_APP").unwrap_or_else(|| "cfmeet-telephony".into()),
            request_timeout: Duration::from_millis(
                get("TELEPHONY_ARI_TIMEOUT_MS")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5_000)
                    .clamp(200, 30_000),
            ),
        })
    }

    fn validate_url(
        url: &str,
        environment: cf_service_auth::RuntimeEnvironment,
    ) -> Result<(), EdgeConfigError> {
        let rest = url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .ok_or_else(|| {
                EdgeConfigError::Invalid("TELEPHONY_ARI_URL", "must be http(s)://host:port".into())
            })?;
        let host = rest.split(['/', ':']).next().unwrap_or("");
        use cf_service_auth::RuntimeEnvironment as E;
        let private = host == "localhost"
            || host.parse::<std::net::IpAddr>().is_ok_and(|ip| match ip {
                std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
                std::net::IpAddr::V6(v6) => v6.is_loopback(),
            })
            || host.ends_with(".internal")
            || host.ends_with(".svc")
            || host.ends_with(".cluster.local");
        if !private && matches!(environment, E::Staging | E::Production) {
            return Err(EdgeConfigError::Invalid(
                "TELEPHONY_ARI_URL",
                "the SIP control plane must be on a private network, never internet-exposed".into(),
            ));
        }
        Ok(())
    }
}

/// Recently ended channels (bounded), so read-back after a lost event can still say
/// "ended" instead of "absent".
#[derive(Default)]
struct History {
    order: VecDeque<String>,
    ended: HashMap<String, (EndReason, DateTime<Utc>)>,
    /// When the event stream last (re)connected; `None` while disconnected.
    connected_since: Option<DateTime<Utc>>,
    /// The latest end time evicted from the bounded history: absence is provable only
    /// for requests sent after it.
    evicted_through: Option<DateTime<Utc>>,
}

const HISTORY_LIMIT: usize = 10_000;

impl History {
    fn record(&mut self, channel: &str, reason: EndReason, at: DateTime<Utc>) {
        if self
            .ended
            .insert(channel.to_string(), (reason, at))
            .is_none()
        {
            self.order.push_back(channel.to_string());
            while self.order.len() > HISTORY_LIMIT {
                if let Some(old) = self.order.pop_front() {
                    if let Some((_, ended_at)) = self.ended.remove(&old) {
                        self.evicted_through = self.evicted_through.max(Some(ended_at));
                    }
                }
            }
        }
    }

    /// Since when every channel end is in this history (see
    /// [`SipMediaEdge::history_complete_since`]).
    fn complete_since(&self) -> Option<DateTime<Utc>> {
        let since = self.connected_since?;
        Some(match self.evicted_through {
            Some(evicted) => since.max(evicted),
            None => since,
        })
    }
}

pub struct AriEdge {
    settings: AriSettings,
    http: reqwest::Client,
    events: broadcast::Sender<EdgeEvent>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    history: Arc<Mutex<History>>,
}

/// Maps a Q.850 cause to the normalized end reason.
pub fn end_reason(cause: u16) -> EndReason {
    match cause {
        16 => EndReason::Completed,
        17 => EndReason::Busy,
        18 | 19 => EndReason::NoAnswer,
        21 => EndReason::Declined,
        _ => EndReason::Failed,
    }
}

/// Maps an ARI channel state to the normalized leg status.
pub fn leg_status(state: &str) -> LegStatus {
    match state {
        "Up" => LegStatus::Connected,
        "Ringing" | "Ring" => LegStatus::Ringing,
        _ => LegStatus::Originating,
    }
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

impl AriEdge {
    /// Starts the adapter and its event WebSocket task (reconnecting with backoff).
    pub fn start(settings: AriSettings) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(settings.request_timeout)
            .connect_timeout(Duration::from_millis(1_000))
            .build()
            .expect("HTTP client");
        let (events, _) = broadcast::channel(4096);
        let edge = Self {
            settings,
            http,
            events,
            connected: Arc::default(),
            history: Arc::default(),
        };
        edge.spawn_event_loop();
        edge
    }

    fn spawn_event_loop(&self) {
        let settings = self.settings.clone();
        let tx = self.events.clone();
        let connected = self.connected.clone();
        let history = self.history.clone();
        let trace = std::env::var("TELEPHONY_ARI_TRACE").as_deref() == Ok("1");
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(250);
            loop {
                let ws_url = format!(
                    "{}/ari/events?app={}&subscribeAll=false",
                    settings.base_url.replacen("http", "ws", 1),
                    settings.app
                );
                let mut request = match tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(ws_url.as_str()) {
                    Ok(r) => r,
                    Err(_) => return,
                };
                use base64::Engine;
                let token = base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    settings.username,
                    settings.password.as_str()
                ));
                if let Ok(v) = format!("Basic {token}").parse() {
                    request.headers_mut().insert("authorization", v);
                }
                match tokio_tungstenite::connect_async(request).await {
                    Ok((mut stream, _)) => {
                        connected.store(true, std::sync::atomic::Ordering::SeqCst);
                        if let Ok(mut h) = history.lock() {
                            h.connected_since = Some(now());
                        }
                        backoff = Duration::from_millis(250);
                        tracing::info!("SIP / media edge event stream connected");
                        while let Some(message) = stream.next().await {
                            let Ok(message) = message else { break };
                            let text = match message {
                                tokio_tungstenite::tungstenite::Message::Text(t) => t,
                                tokio_tungstenite::tungstenite::Message::Close(_) => break,
                                _ => continue,
                            };
                            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                                continue;
                            };
                            if trace {
                                // Diagnostics only: event type and channel state, never caller data.
                                eprintln!(
                                    "ari event: {} {}",
                                    value["type"].as_str().unwrap_or("?"),
                                    value["channel"]["state"].as_str().unwrap_or("")
                                );
                            }
                            if let Some(event) = translate(&value, &history) {
                                let _ = tx.send(event);
                            }
                        }
                        connected.store(false, std::sync::atomic::Ordering::SeqCst);
                        if let Ok(mut h) = history.lock() {
                            h.connected_since = None;
                        }
                        tracing::warn!("SIP / media edge event stream disconnected; reconnecting");
                    }
                    Err(_) => {
                        connected.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        });
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> EdgeResult<(StatusCode, Value)> {
        let url = format!("{}/ari{path}", self.settings.base_url);
        let mut request = self
            .http
            .request(method, url)
            .basic_auth(
                &self.settings.username,
                Some(self.settings.password.as_str()),
            )
            .query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| {
            if e.is_connect() {
                EdgeError::Unavailable
            } else {
                EdgeError::Timeout
            }
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|_| EdgeError::Timeout)?;
        let value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        match status {
            s if s.is_success() => Ok((s, value)),
            StatusCode::NOT_FOUND => Err(EdgeError::NotFound),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(EdgeError::Unavailable),
            s if s.is_client_error() => Err(EdgeError::Refused),
            _ => Err(EdgeError::Timeout),
        }
    }

    /// A raw (binary) GET bounded to `max_bytes`.
    async fn fetch_bytes(&self, path: &str, max_bytes: usize) -> EdgeResult<Vec<u8>> {
        let url = format!("{}/ari{path}", self.settings.base_url);
        let mut response = self
            .http
            .get(url)
            .basic_auth(
                &self.settings.username,
                Some(self.settings.password.as_str()),
            )
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    EdgeError::Unavailable
                } else {
                    EdgeError::Timeout
                }
            })?;
        match response.status() {
            s if s.is_success() => {}
            StatusCode::NOT_FOUND => return Err(EdgeError::NotFound),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(EdgeError::Unavailable),
            s if s.is_client_error() => return Err(EdgeError::Refused),
            _ => return Err(EdgeError::Timeout),
        }
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| EdgeError::Timeout)? {
            if out.len() + chunk.len() > max_bytes {
                return Err(EdgeError::Refused);
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    fn valid_ref(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 128
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    }
}

/// Part F: a spool recording's end (its target channel is in `target_uri`).
fn translate_recording(kind: &str, value: &Value) -> Option<EdgeEvent> {
    let recording = &value["recording"];
    let name = recording["name"].as_str()?;
    // The recorded channel, or (a call recording of a bridged leg) the bridge.
    let target = recording["target_uri"].as_str()?;
    let channel = target
        .strip_prefix("channel:")
        .or_else(|| target.strip_prefix("bridge:"))?;
    if !AriEdge::valid_ref(name) || !AriEdge::valid_ref(channel) {
        return None;
    }
    let at = value["timestamp"]
        .as_str()
        .and_then(|t| DateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.3f%z").ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(now);
    let edge_kind = if kind == "RecordingFinished" {
        EdgeEventKind::CaptureFinished {
            name: name.to_string(),
        }
    } else {
        EdgeEventKind::CaptureFailed {
            name: name.to_string(),
        }
    };
    let digest = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(format!("{kind}|{name}").as_bytes()))
    };
    Some(EdgeEvent {
        event_id: format!("ari-{}", &digest[..40]),
        application_media: false,
        channel: channel.to_string(),
        correlation_key: None,
        kind: edge_kind,
        at,
    })
}

/// Translates one ARI event into a neutral edge event (None: not relevant).
fn translate(value: &Value, history: &Mutex<History>) -> Option<EdgeEvent> {
    let kind = value["type"].as_str()?;
    if matches!(kind, "RecordingFinished" | "RecordingFailed") {
        return translate_recording(kind, value);
    }
    let channel = &value["channel"];
    let channel_id = channel["id"].as_str()?.to_string();
    let at = value["timestamp"]
        .as_str()
        .and_then(|t| DateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.3f%z").ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(now);
    let correlation = channel_id.starts_with("cfm-").then(|| channel_id.clone());
    let edge_kind = match kind {
        "StasisStart" => {
            let args: Vec<&str> = value["args"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            match args.first().copied() {
                Some("inbound") => {
                    let name = channel["name"].as_str().unwrap_or("");
                    let peer = name
                        .strip_prefix("PJSIP/")
                        .and_then(|n| n.rsplit_once('-').map(|(p, _)| p.to_string()));
                    EdgeEventKind::Inbound(EdgeInbound {
                        peer,
                        called: ExecutionNumber::parse(
                            channel["dialplan"]["exten"].as_str().unwrap_or(""),
                        ),
                        caller: ExecutionNumber::parse(
                            channel["caller"]["number"].as_str().unwrap_or(""),
                        ),
                        media_join_code: None,
                    })
                }
                Some("appmedia") => {
                    let code = channel["dialplan"]["exten"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let valid = code.starts_with("cfmj")
                        && code.len() <= 64
                        && code.bytes().all(|b| b.is_ascii_alphanumeric());
                    EdgeEventKind::Inbound(EdgeInbound {
                        peer: None,
                        called: None,
                        caller: None,
                        media_join_code: Some(if valid { code } else { String::new() }),
                    })
                }
                // Part F: an enterprise device dialled an internal extension. The
                // endpoint is the authenticated PJSIP endpoint (never a caller-supplied
                // name); the extension a short code (digits or a lower-case word).
                Some("enterprise") => {
                    let name = channel["name"].as_str().unwrap_or("");
                    let endpoint = name
                        .strip_prefix("PJSIP/")
                        .and_then(|n| n.rsplit_once('-').map(|(p, _)| p.to_string()))
                        .unwrap_or_default();
                    let exten = channel["dialplan"]["exten"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let valid_exten = (2..=32).contains(&exten.len())
                        && (exten.bytes().all(|b| b.is_ascii_digit()) && exten.len() <= 6
                            || exten.as_bytes()[0].is_ascii_lowercase()
                                && exten.bytes().all(|b| {
                                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
                                }));
                    EdgeEventKind::EnterpriseInbound(EdgeEnterpriseDialog {
                        endpoint,
                        extension: if valid_exten { exten } else { String::new() },
                    })
                }
                // An originated channel enters the application when it is answered.
                _ => EdgeEventKind::OriginatedAnswered,
            }
        }
        "ChannelStateChange" => match channel["state"].as_str()? {
            "Ringing" => EdgeEventKind::Ringing,
            "Up" => EdgeEventKind::Answered,
            _ => return None,
        },
        // Dialplan-entered (inbound) channels leave the application at StasisEnd, before
        // ChannelDestroyed: the hangup request is the reliable end signal for every channel.
        "ChannelDestroyed" | "ChannelHangupRequest" => {
            let cause = value["cause"].as_u64().unwrap_or(0) as u16;
            let mut reason = end_reason(cause);
            // Torn down while still ringing (ring timeout / cancel): never answered.
            if matches!(channel["state"].as_str(), Some("Ringing" | "Ring"))
                && matches!(reason, EndReason::Failed | EndReason::Completed)
            {
                reason = EndReason::NoAnswer;
            }
            if let Ok(mut h) = history.lock() {
                h.record(&channel_id, reason, at);
            }
            EdgeEventKind::Hangup { reason, cause }
        }
        "ChannelDtmfReceived" => EdgeEventKind::Dtmf {
            digit: value["digit"].as_str()?.chars().next()?,
        },
        "ChannelHold" => EdgeEventKind::Held,
        "ChannelUnhold" => EdgeEventKind::Unheld,
        "ChannelEnteredBridge" => EdgeEventKind::EnteredBridge {
            bridge: value["bridge"]["id"].as_str()?.to_string(),
        },
        "ChannelLeftBridge" => EdgeEventKind::LeftBridge {
            bridge: value["bridge"]["id"].as_str()?.to_string(),
        },
        _ => return None,
    };
    // A stable, opaque, constraint-safe dedupe key for this event.
    let stamp = value["timestamp"].as_str().unwrap_or("");
    let digest = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            format!(
                "{kind}|{channel_id}|{stamp}|{}",
                value["digit"].as_str().unwrap_or("")
            )
            .as_bytes(),
        ))
    };
    let application_media = channel["name"]
        .as_str()
        .is_some_and(|n| n.starts_with("PJSIP/app-media-"));
    Some(EdgeEvent {
        event_id: format!("ari-{}", &digest[..40]),
        application_media,
        channel: channel_id,
        correlation_key: correlation,
        kind: edge_kind,
        at,
    })
}

#[async_trait]
impl SipMediaEdge for AriEdge {
    fn name(&self) -> &'static str {
        "asterisk-ari"
    }

    async fn health(&self) -> Health {
        match self.call(Method::GET, "/asterisk/info", &[], None).await {
            Ok(_) if self.connected.load(std::sync::atomic::Ordering::SeqCst) => Health::Healthy,
            Ok(_) => Health::Degraded,
            Err(_) => Health::Unavailable,
        }
    }

    async fn originate(&self, request: &EdgeOriginate) -> EdgeResult<ChannelRef> {
        if !Self::valid_ref(&request.trunk) || !Self::valid_ref(&request.correlation_key) {
            return Err(EdgeError::Refused);
        }
        let endpoint = format!("PJSIP/{}@{}", request.destination.expose(), request.trunk);
        let query = [
            ("endpoint", endpoint),
            ("app", self.settings.app.clone()),
            ("appArgs", "outbound".to_string()),
            ("callerId", request.caller_identity.expose().to_string()),
            ("timeout", request.ring_timeout_seconds.to_string()),
            ("channelId", request.correlation_key.clone()),
        ];
        let (_, value) = self.call(Method::POST, "/channels", &query, None).await?;
        match value["id"].as_str() {
            Some(id) if id == request.correlation_key => Ok(id.to_string()),
            _ => Err(EdgeError::Timeout),
        }
    }

    async fn ring(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.call(
            Method::POST,
            &format!("/channels/{channel}/ring"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn answer(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.call(
            Method::POST,
            &format!("/channels/{channel}/answer"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn hangup(&self, channel: &ChannelRef) -> EdgeResult<()> {
        if !Self::valid_ref(channel) {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::DELETE,
            &format!("/channels/{channel}"),
            &[("reason_code", "16".into())],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn reject(&self, channel: &ChannelRef) -> EdgeResult<()> {
        if !Self::valid_ref(channel) {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::DELETE,
            &format!("/channels/{channel}"),
            &[("reason_code", "21".into())],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn create_bridge(&self, name: &str) -> EdgeResult<String> {
        if !Self::valid_ref(name) {
            return Err(EdgeError::Refused);
        }
        let (_, value) = self
            .call(
                Method::POST,
                "/bridges",
                &[
                    ("type", "mixing".into()),
                    ("bridgeId", name.to_string()),
                    ("name", name.to_string()),
                ],
                None,
            )
            .await?;
        value["id"]
            .as_str()
            .map(str::to_string)
            .ok_or(EdgeError::Timeout)
    }

    async fn add_to_bridge(&self, bridge: &str, channel: &ChannelRef) -> EdgeResult<()> {
        self.call(
            Method::POST,
            &format!("/bridges/{bridge}/addChannel"),
            &[("channel", channel.clone())],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn remove_from_bridge(&self, bridge: &str, channel: &ChannelRef) -> EdgeResult<()> {
        self.call(
            Method::POST,
            &format!("/bridges/{bridge}/removeChannel"),
            &[("channel", channel.clone())],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn destroy_bridge(&self, bridge: &str) -> EdgeResult<()> {
        self.call(Method::DELETE, &format!("/bridges/{bridge}"), &[], None)
            .await
            .map(|_| ())
    }

    async fn hold(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.call(
            Method::POST,
            &format!("/channels/{channel}/hold"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn resume(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.call(
            Method::DELETE,
            &format!("/channels/{channel}/hold"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn send_dtmf(&self, channel: &ChannelRef, digits: &str) -> EdgeResult<()> {
        if digits.is_empty()
            || digits.len() > 32
            || !digits
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'*' | b'#' | b'A'..=b'D'))
        {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::POST,
            &format!("/channels/{channel}/dtmf"),
            &[("dtmf", digits.to_string())],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn redirect(&self, channel: &ChannelRef, endpoint: &str) -> EdgeResult<()> {
        self.call(
            Method::POST,
            &format!("/channels/{channel}/redirect"),
            &[("endpoint", endpoint.to_string())],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn record(
        &self,
        channel: &ChannelRef,
        name: &str,
        max_seconds: u32,
        beep: bool,
    ) -> EdgeResult<()> {
        if !Self::valid_ref(channel) || !Self::valid_ref(name) || max_seconds == 0 {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::POST,
            &format!("/channels/{channel}/record"),
            &[
                ("name", name.to_string()),
                ("format", "wav".to_string()),
                ("maxDurationSeconds", max_seconds.to_string()),
                ("maxSilenceSeconds", "0".to_string()),
                ("ifExists", "overwrite".to_string()),
                ("beep", beep.to_string()),
                ("terminateOn", "#".to_string()),
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn record_bridge(
        &self,
        bridge: &str,
        name: &str,
        max_seconds: u32,
        beep: bool,
    ) -> EdgeResult<()> {
        if !Self::valid_ref(bridge) || !Self::valid_ref(name) || max_seconds == 0 {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::POST,
            &format!("/bridges/{bridge}/record"),
            &[
                ("name", name.to_string()),
                ("format", "wav".to_string()),
                ("maxDurationSeconds", max_seconds.to_string()),
                ("maxSilenceSeconds", "0".to_string()),
                ("ifExists", "overwrite".to_string()),
                ("beep", beep.to_string()),
                ("terminateOn", "none".to_string()),
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn stop_recording(&self, name: &str) -> EdgeResult<()> {
        if !Self::valid_ref(name) {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::POST,
            &format!("/recordings/live/{name}/stop"),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn fetch_recording(&self, name: &str, max_bytes: usize) -> EdgeResult<Vec<u8>> {
        if !Self::valid_ref(name) {
            return Err(EdgeError::Refused);
        }
        self.fetch_bytes(&format!("/recordings/stored/{name}/file"), max_bytes)
            .await
    }

    async fn delete_recording(&self, name: &str) -> EdgeResult<()> {
        if !Self::valid_ref(name) {
            return Err(EdgeError::Refused);
        }
        match self
            .call(
                Method::DELETE,
                &format!("/recordings/stored/{name}"),
                &[],
                None,
            )
            .await
        {
            Ok(_) | Err(EdgeError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn play(&self, channel: &ChannelRef, sound: &str) -> EdgeResult<()> {
        if !Self::valid_ref(channel)
            || sound.is_empty()
            || sound.len() > 64
            || !sound
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(EdgeError::Refused);
        }
        self.call(
            Method::POST,
            &format!("/channels/{channel}/play"),
            &[("media", format!("sound:{sound}"))],
            None,
        )
        .await
        .map(|_| ())
    }

    async fn query(&self, channel: &ChannelRef) -> ChannelState {
        match self
            .call(Method::GET, &format!("/channels/{channel}"), &[], None)
            .await
        {
            Ok((_, value)) => {
                ChannelState::Active(leg_status(value["state"].as_str().unwrap_or("")))
            }
            Err(EdgeError::NotFound) => ChannelState::Gone,
            Err(_) => ChannelState::Unknown,
        }
    }

    async fn find_by_correlation(
        &self,
        correlation_key: &str,
    ) -> EdgeResult<Option<ChannelSummary>> {
        if !Self::valid_ref(correlation_key) {
            return Err(EdgeError::Refused);
        }
        match self
            .call(
                Method::GET,
                &format!("/channels/{correlation_key}"),
                &[],
                None,
            )
            .await
        {
            Ok((_, value)) => Ok(Some(ChannelSummary {
                channel: correlation_key.to_string(),
                correlation_key: Some(correlation_key.to_string()),
                status: leg_status(value["state"].as_str().unwrap_or("")),
            })),
            Err(EdgeError::NotFound) => {
                let ended = self
                    .history
                    .lock()
                    .ok()
                    .and_then(|h| h.ended.get(correlation_key).copied());
                Ok(ended.map(|(reason, _)| ChannelSummary {
                    channel: correlation_key.to_string(),
                    correlation_key: Some(correlation_key.to_string()),
                    status: if reason == EndReason::Completed {
                        LegStatus::Ended
                    } else {
                        LegStatus::Failed
                    },
                }))
            }
            Err(e) => Err(e),
        }
    }

    fn history_complete_since(&self) -> Option<DateTime<Utc>> {
        self.history.lock().ok().and_then(|h| h.complete_since())
    }

    async fn list(&self) -> EdgeResult<Vec<ChannelSummary>> {
        let (_, value) = self.call(Method::GET, "/channels", &[], None).await?;
        Ok(value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| {
                        let id = c["id"].as_str()?.to_string();
                        Some(ChannelSummary {
                            correlation_key: id.starts_with("cfm-").then(|| id.clone()),
                            channel: id,
                            status: leg_status(c["state"].as_str().unwrap_or("")),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn media_stats(&self, channel: &ChannelRef) -> Option<(u64, u64)> {
        let (_, value) = self
            .call(
                Method::GET,
                &format!("/channels/{channel}/rtp_statistics"),
                &[],
                None,
            )
            .await
            .ok()?;
        Some((value["rxcount"].as_u64()?, value["txcount"].as_u64()?))
    }

    fn subscribe(&self) -> broadcast::Receiver<EdgeEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn causes_and_states_map_to_the_neutral_vocabulary() {
        assert_eq!(end_reason(16), EndReason::Completed);
        assert_eq!(end_reason(17), EndReason::Busy);
        assert_eq!(end_reason(19), EndReason::NoAnswer);
        assert_eq!(end_reason(21), EndReason::Declined);
        assert_eq!(end_reason(34), EndReason::Failed);
        assert_eq!(leg_status("Up"), LegStatus::Connected);
        assert_eq!(leg_status("Ringing"), LegStatus::Ringing);
    }

    #[test]
    fn events_translate_with_peer_called_and_join_codes() {
        let h = Mutex::new(History::default());
        let inbound = translate(
            &json!({ "type": "StasisStart", "timestamp": "2026-10-08T10:00:00.000+0000", "args": ["inbound"],
                     "channel": { "id": "1696.1", "name": "PJSIP/peer-ng-a-00000001", "state": "Ring",
                                  "caller": { "number": "+2348031234567" },
                                  "dialplan": { "exten": "+2342010000001", "context": "from-trunk" } } }),
            &h,
        )
        .unwrap();
        match inbound.kind {
            EdgeEventKind::Inbound(i) => {
                assert_eq!(i.peer.as_deref(), Some("peer-ng-a"));
                assert_eq!(i.called.unwrap().expose(), "+2342010000001");
                assert!(i.caller.is_some());
            }
            other => panic!("{other:?}"),
        }
        let join = translate(
            &json!({ "type": "StasisStart", "timestamp": "2026-10-08T10:00:00.000+0000", "args": ["appmedia"],
                     "channel": { "id": "1696.2", "name": "PJSIP/app-media-00000002", "dialplan": { "exten": "cfmjabc123" } } }),
            &h,
        )
        .unwrap();
        assert!(
            matches!(join.kind, EdgeEventKind::Inbound(EdgeInbound { media_join_code: Some(ref c), .. }) if c == "cfmjabc123")
        );
        let busy = translate(
            &json!({ "type": "ChannelDestroyed", "timestamp": "2026-10-08T10:00:01.000+0000", "cause": 17,
                     "channel": { "id": "cfm-0123456789abcdef", "state": "Down" } }),
            &h,
        )
        .unwrap();
        assert!(matches!(
            busy.kind,
            EdgeEventKind::Hangup {
                reason: EndReason::Busy,
                cause: 17
            }
        ));
        assert_eq!(
            busy.correlation_key.as_deref(),
            Some("cfm-0123456789abcdef")
        );
        assert!(h.lock().unwrap().ended.contains_key("cfm-0123456789abcdef"));
    }

    #[test]
    fn recording_ends_translate_by_spool_name_and_target_channel() {
        let h = Mutex::new(History::default());
        let finished = translate(
            &json!({ "type": "RecordingFinished", "timestamp": "2026-10-09T10:00:00.000+0000",
                     "recording": { "name": "cfm-cap-0123456789abcdef0123456789abcdef", "format": "wav",
                                    "state": "done", "target_uri": "channel:1760000000.42" } }),
            &h,
        )
        .unwrap();
        assert_eq!(finished.channel, "1760000000.42");
        assert!(matches!(
            &finished.kind,
            EdgeEventKind::CaptureFinished { name } if name == "cfm-cap-0123456789abcdef0123456789abcdef"
        ));
        let failed = translate(
            &json!({ "type": "RecordingFailed", "recording": { "name": "cfm-cap-x1",
                     "target_uri": "channel:abc" } }),
            &h,
        )
        .unwrap();
        assert!(matches!(failed.kind, EdgeEventKind::CaptureFailed { .. }));
        // A bridge recording ends the same way.
        let bridged = translate(
            &json!({ "type": "RecordingFinished", "recording": { "name": "cfm-cap-x2",
                     "target_uri": "bridge:cfmb-1" } }),
            &h,
        )
        .unwrap();
        assert!(matches!(
            bridged.kind,
            EdgeEventKind::CaptureFinished { .. }
        ));
        // Not a channel / bridge target, or an unsafe name: ignored.
        for value in [
            json!({ "type": "RecordingFinished", "recording": { "name": "cfm-cap-x1", "target_uri": "endpoint:x" } }),
            json!({ "type": "RecordingFinished", "recording": { "name": "../../etc/passwd", "target_uri": "channel:c" } }),
        ] {
            assert!(translate(&value, &h).is_none());
        }
    }

    #[test]
    fn the_control_plane_must_stay_private_outside_local_and_test() {
        use cf_service_auth::RuntimeEnvironment as E;
        assert!(AriSettings::validate_url("http://127.0.0.1:8088", E::Production).is_ok());
        assert!(AriSettings::validate_url("http://10.0.4.2:8088", E::Production).is_ok());
        assert!(
            AriSettings::validate_url("http://asterisk.telephony.svc:8088", E::Staging).is_ok()
        );
        assert!(AriSettings::validate_url("http://203.0.113.9:8088", E::Production).is_err());
        assert!(AriSettings::validate_url("http://edge.example.com:8088", E::Production).is_err());
        assert!(AriSettings::validate_url("http://edge.example.com:8088", E::Local).is_ok());
    }

    #[test]
    fn absence_is_provable_only_inside_the_observed_window() {
        let t = |s: i64| DateTime::<Utc>::from_timestamp(1_790_000_000 + s, 0).unwrap();
        let mut h = History::default();
        assert_eq!(h.complete_since(), None, "never connected: no proof");
        h.connected_since = Some(t(0));
        assert_eq!(h.complete_since(), Some(t(0)));
        for n in 0..=HISTORY_LIMIT {
            h.record(
                &format!("cfm-{n:016x}"),
                EndReason::Completed,
                t(10 + n as i64),
            );
        }
        // The oldest end (t(10)) was evicted: proof now starts after it.
        assert_eq!(h.complete_since(), Some(t(10)));
        h.connected_since = None;
        assert_eq!(h.complete_since(), None, "disconnected: no proof");
    }
}
