//! Self-hosted LiveKit adapter of the Calling media-provider contract (Part C §24).
//!
//! LiveKit is the current adapter, not the architecture: this file is the only place in
//! the Calling service that knows LiveKit's wire formats. It speaks the LiveKit server
//! API directly (Twirp JSON over HTTP, HS256 access tokens, signed webhooks) — no Meeting
//! Service source or type is used, and no provider SDK is linked.
//!
//! Configuration is Calling's own (`CALLING_LIVEKIT_*`). The API secret lives in a
//! redacted container: it is never logged, serialized, persisted or returned, and the
//! configuration type has no derived `Debug`.
//!
//! Credentials (§42–§44; ADR 0007):
//!
//! * clients never hold a token LiveKit accepts. Their media credential is a LiveKit-shaped
//!   JWT sealed with Calling's separate media-edge key (`CALLING_MEDIA_EDGE_SIGNING_KEY`,
//!   issuer [`EDGE_ISSUER`], unknown to LiveKit), usable only through Calling's media
//!   connection gateway (`livekit_gateway`), which admits every connection against the
//!   database first;
//! * the gateway opens each admitted upstream connection with a token minted for that one
//!   connection: `roomJoin` for exactly the Call's canonical room and the session's media
//!   identity, and NOTHING else — no publish, no subscribe, no data, no metadata updates,
//!   no admin / create / list / record / ingress / agent grant. The session's media
//!   generation is carried as the participant attribute `cfm.gen`;
//! * the server grants the effective media permissions with `UpdateParticipant` only after
//!   it verified the connection against the database. The privileged tokens LiveKit then
//!   refreshes are re-sealed with the edge key on their way to the client, so a saved
//!   refreshed token is again only usable through the gateway;
//! * server API calls use separate short-lived tokens with the minimal grant per call.

use std::{collections::BTreeMap, fmt, time::Duration as StdDuration};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Duration, TimeZone, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    gateway::{ConnectionClaims, ConnectionDenied},
    permissions::MediaPermissions,
    provider::{
        CallMediaProvider, EnsuredMediaSession, EventVerificationError, IssuedMediaCredential,
        MediaCredentialRequest, MediaParticipantRef, MediaProviderCapabilities, MediaProviderError,
        MediaSessionRef, MediaSessionSpec, MediaToken, PermissionOutcome, ProviderParticipant,
        ProviderSessionSnapshot, RemovalOutcome, TerminationOutcome, VerifiedMediaEvent,
    },
};
use crate::{calls::kinds::MediaEventKind, config::ConfigError};

/// Adapter name recorded in Calling's private media binding.
pub const ADAPTER_NAME: &str = "livekit";
/// Participant attribute carrying the session media generation.
pub const GENERATION_ATTRIBUTE: &str = "cfm.gen";
/// Issuer of client media credentials (Calling's media edge). Never a LiveKit API key.
pub const EDGE_ISSUER: &str = "cfmeet-media-edge";
/// Lifetime of the per-connection upstream token (used once, by the gateway, to connect).
const UPSTREAM_TOKEN_TTL_SECONDS: i64 = 30;
/// Longest lifetime a re-sealed refreshed credential may carry.
const MAX_RESEALED_TTL_SECONDS: i64 = 900;

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MIN_SECRET_LEN: usize = 32;
/// Lifetime of the server's own API tokens.
const ADMIN_TOKEN_TTL_SECONDS: i64 = 60;
/// Accepted clock skew for webhook tokens.
const WEBHOOK_LEEWAY_SECONDS: u64 = 30;

/// Calling's LiveKit configuration. No `Debug` derive: the secrets are redacted by hand.
pub struct LiveKitSettings {
    /// LiveKit's signaling URL the gateway relays to (`wss://…`, `ws://` only for loopback).
    /// Never handed to clients.
    signal_url: String,
    /// Public URL of Calling's media connection gateway, handed to clients.
    gateway_url: String,
    /// Server API base URL (`https://…`, `http://` only for loopback development).
    api_url: String,
    api_key: String,
    api_secret: SecretString,
    /// Calling's media-edge key sealing client credentials (unknown to LiveKit).
    edge_key: SecretString,
    api_timeout: StdDuration,
    /// LiveKit SIP outbound trunk that reaches the SIP / media edge's internal media peer
    /// (`CALLING_LIVEKIT_SIP_TRUNK_ID`). Without it no public-network leg can be bridged
    /// into an application call (public routes then fail safely).
    sip_trunk_id: Option<String>,
}

impl fmt::Debug for LiveKitSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveKitSettings")
            .field("signal_url", &self.signal_url)
            .field("gateway_url", &self.gateway_url)
            .field("api_url", &self.api_url)
            .field("api_key", &"[REDACTED]")
            .field("api_secret", &"[REDACTED]")
            .field("edge_key", &"[REDACTED]")
            .field("api_timeout", &self.api_timeout)
            .field("sip_trunk_configured", &self.sip_trunk_id.is_some())
            .finish()
    }
}

impl LiveKitSettings {
    /// A copy for building an adapter (the secret stays in a redacted container).
    pub(crate) fn clone_settings(&self) -> Self {
        Self {
            signal_url: self.signal_url.clone(),
            gateway_url: self.gateway_url.clone(),
            api_url: self.api_url.clone(),
            api_key: self.api_key.clone(),
            api_secret: self.api_secret.clone(),
            edge_key: self.edge_key.clone(),
            api_timeout: self.api_timeout,
            sip_trunk_id: self.sip_trunk_id.clone(),
        }
    }

    pub fn sip_trunk_id(&self) -> Option<&str> {
        self.sip_trunk_id.as_deref()
    }

    /// Reads `CALLING_LIVEKIT_URL` (LiveKit signaling, gateway upstream),
    /// `CALLING_LIVEKIT_API_URL`, `CALLING_LIVEKIT_API_KEY`, `CALLING_LIVEKIT_API_SECRET`,
    /// `CALLING_MEDIA_GATEWAY_URL` (public gateway URL), `CALLING_MEDIA_EDGE_SIGNING_KEY`
    /// and `CALLING_LIVEKIT_API_TIMEOUT_MS`. Every value is required (no fallback
    /// credential exists); invalid values are refused without echoing them.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let required = |var: &'static str| {
            get(var)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or(ConfigError::Missing { var })
        };
        let signal_url = required("CALLING_LIVEKIT_URL")?
            .trim_end_matches('/')
            .to_string();
        check_url("CALLING_LIVEKIT_URL", &signal_url, &["wss://"], &["ws://"])?;
        let gateway_url = required("CALLING_MEDIA_GATEWAY_URL")?
            .trim_end_matches('/')
            .to_string();
        check_url(
            "CALLING_MEDIA_GATEWAY_URL",
            &gateway_url,
            &["wss://"],
            &["ws://"],
        )?;
        let api_url = required("CALLING_LIVEKIT_API_URL")?
            .trim_end_matches('/')
            .to_string();
        check_url(
            "CALLING_LIVEKIT_API_URL",
            &api_url,
            &["https://"],
            &["http://"],
        )?;
        let api_key = required("CALLING_LIVEKIT_API_KEY")?;
        if api_key.len() > 64
            || api_key == EDGE_ISSUER
            || !api_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(ConfigError::invalid(
                "CALLING_LIVEKIT_API_KEY",
                "must be 1-64 characters of [A-Za-z0-9_-]".to_string(),
            ));
        }
        let api_secret = required("CALLING_LIVEKIT_API_SECRET")?;
        let lowered = api_secret.to_ascii_lowercase();
        if api_secret.len() < MIN_SECRET_LEN
            || ["changeme", "change-me", "placeholder", "example", "your-"]
                .iter()
                .any(|p| lowered.contains(p))
        {
            return Err(ConfigError::invalid(
                "CALLING_LIVEKIT_API_SECRET",
                format!("must be a generated secret of at least {MIN_SECRET_LEN} characters"),
            ));
        }
        let edge_key = required("CALLING_MEDIA_EDGE_SIGNING_KEY")?;
        if edge_key.len() < MIN_SECRET_LEN || edge_key == api_secret {
            return Err(ConfigError::invalid(
                "CALLING_MEDIA_EDGE_SIGNING_KEY",
                format!(
                    "must be a generated secret of at least {MIN_SECRET_LEN} characters, distinct from the LiveKit API secret"
                ),
            ));
        }
        let api_timeout = match get("CALLING_LIVEKIT_API_TIMEOUT_MS") {
            None => DEFAULT_TIMEOUT_MS,
            Some(v) => v
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|ms| (100..=MAX_TIMEOUT_MS).contains(ms))
                .ok_or_else(|| {
                    ConfigError::invalid(
                        "CALLING_LIVEKIT_API_TIMEOUT_MS",
                        format!("must be an integer between 100 and {MAX_TIMEOUT_MS}"),
                    )
                })?,
        };
        let sip_trunk_id = match get("CALLING_LIVEKIT_SIP_TRUNK_ID").map(|v| v.trim().to_string()) {
            None => None,
            Some(v) if v.is_empty() => None,
            Some(v)
                if v.len() <= 64
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
            {
                Some(v)
            }
            Some(_) => {
                return Err(ConfigError::invalid(
                    "CALLING_LIVEKIT_SIP_TRUNK_ID",
                    "must be 1-64 characters of [A-Za-z0-9_-]".to_string(),
                ))
            }
        };
        Ok(Self {
            signal_url,
            gateway_url,
            api_url,
            api_key,
            api_secret: SecretString::from(api_secret),
            edge_key: SecretString::from(edge_key),
            api_timeout: StdDuration::from_millis(api_timeout),
            sip_trunk_id,
        })
    }
}

/// `https`/`wss` everywhere; plain `http`/`ws` only for loopback hosts (development and
/// the RTC certification suite).
fn check_url(
    var: &'static str,
    url: &str,
    secure: &[&str],
    insecure: &[&str],
) -> Result<(), ConfigError> {
    let lower = url.to_ascii_lowercase();
    let host_of = |rest: &str| {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = if authority.starts_with('[') {
            authority.split(']').next().map(|h| format!("{h}]"))
        } else {
            authority.split(':').next().map(str::to_string)
        };
        host.unwrap_or_default()
    };
    if let Some(prefix) = secure.iter().find(|p| lower.starts_with(**p)) {
        if host_of(&lower[prefix.len()..]).is_empty() {
            return Err(ConfigError::invalid(var, "has no host".to_string()));
        }
        return Ok(());
    }
    if let Some(prefix) = insecure.iter().find(|p| lower.starts_with(**p)) {
        let host = host_of(&lower[prefix.len()..]);
        if matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(());
        }
        return Err(ConfigError::invalid(
            var,
            "must use TLS (plain transport is allowed only for loopback hosts)".to_string(),
        ));
    }
    Err(ConfigError::invalid(
        var,
        "has an unsupported scheme".to_string(),
    ))
}

/// The LiveKit adapter.
pub struct LiveKitAdapter {
    settings: LiveKitSettings,
    http: reqwest::Client,
}

impl fmt::Debug for LiveKitAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveKitAdapter")
            .field("settings", &self.settings)
            .finish()
    }
}

/// HS256 claims of a LiveKit access token.
#[derive(Serialize)]
struct AccessClaims<'a> {
    iss: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sub: Option<&'a str>,
    nbf: i64,
    exp: i64,
    video: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    sip: Option<Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    attributes: BTreeMap<&'a str, String>,
}

/// Which key signs a token.
#[derive(Debug, Clone, Copy)]
enum Signer {
    /// LiveKit's API key: server API calls and per-connection upstream tokens.
    LiveKit,
    /// Calling's media edge: client credentials (LiveKit does not know this key).
    Edge,
}

#[derive(Deserialize)]
struct WebhookClaims {
    #[serde(default)]
    sha256: String,
}

impl LiveKitAdapter {
    pub fn new(settings: LiveKitSettings) -> Result<Self, ConfigError> {
        let http = reqwest::Client::builder()
            .timeout(settings.api_timeout)
            .connect_timeout(settings.api_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                ConfigError::invalid(
                    "CALLING_LIVEKIT_API_URL",
                    "cannot build the HTTP client".to_string(),
                )
            })?;
        Ok(Self { settings, http })
    }

    fn key(&self, signer: Signer) -> (&str, &SecretString) {
        match signer {
            Signer::LiveKit => (self.settings.api_key.as_str(), &self.settings.api_secret),
            Signer::Edge => (EDGE_ISSUER, &self.settings.edge_key),
        }
    }

    fn sign(
        &self,
        signer: Signer,
        identity: Option<&str>,
        ttl: Duration,
        video: Value,
        attributes: BTreeMap<&str, String>,
    ) -> Result<(String, DateTime<Utc>), MediaProviderError> {
        let now = Utc::now();
        let expires_at = now + ttl;
        let (issuer, secret) = self.key(signer);
        let claims = AccessClaims {
            iss: issuer,
            sub: identity,
            // LiveKit validates nbf strictly; allow for small clock skew between hosts.
            nbf: now.timestamp() - 5,
            exp: expires_at.timestamp(),
            video,
            sip: None,
            attributes,
        };
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.expose_secret().as_bytes()),
        )
        .map_err(|_| MediaProviderError::Rejected)?;
        Ok((token, expires_at))
    }

    /// The join-only grant: every capability explicitly false (LiveKit treats an absent
    /// publish / subscribe / data grant as TRUE).
    fn join_only(room: &str) -> Value {
        json!({
            "room": room,
            "roomJoin": true,
            "roomCreate": false,
            "roomList": false,
            "roomRecord": false,
            "roomAdmin": false,
            "ingressAdmin": false,
            "canPublish": false,
            "canSubscribe": false,
            "canPublishData": false,
            "canPublishSources": [],
            "canUpdateOwnMetadata": false,
            "canSubscribeMetrics": false,
            "hidden": false,
            "recorder": false,
            "agent": false,
        })
    }

    fn generation_attributes(generation: Option<i32>) -> BTreeMap<&'static str, String> {
        let mut attributes = BTreeMap::new();
        if let Some(generation) = generation {
            attributes.insert(GENERATION_ATTRIBUTE, generation.to_string());
        }
        attributes
    }

    /// LiveKit's signaling base URL (gateway upstream only; never handed to clients).
    pub fn signal_url(&self) -> &str {
        &self.settings.signal_url
    }

    /// A token for ONE admitted upstream connection: join-only, the claimed room, identity
    /// and generation, 30 s. Minted by the gateway after admission; never leaves it.
    pub fn upstream_token(&self, claims: &ConnectionClaims) -> Result<String, MediaProviderError> {
        self.sign(
            Signer::LiveKit,
            Some(&claims.identity),
            Duration::seconds(UPSTREAM_TOKEN_TTL_SECONDS),
            Self::join_only(&claims.resource_name),
            Self::generation_attributes(claims.generation),
        )
        .map(|(t, _)| t)
    }

    fn decode_claims(
        token: &str,
        issuer: &str,
        secret: &SecretString,
        now: DateTime<Utc>,
    ) -> Option<Value> {
        let header = jsonwebtoken::decode_header(token).ok()?;
        if header.alg != Algorithm::HS256 {
            return None;
        }
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "sub"]);
        // Time is checked below, explicitly, against the caller's clock.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        let claims = jsonwebtoken::decode::<Value>(
            token,
            &DecodingKey::from_secret(secret.expose_secret().as_bytes()),
            &validation,
        )
        .ok()?
        .claims;
        let exp = claims.get("exp").and_then(Value::as_i64)?;
        let nbf = claims
            .get("nbf")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MIN);
        let now = now.timestamp();
        (exp > now && nbf <= now + 5).then_some(claims)
    }

    fn connection_claims(claims: &Value) -> Option<ConnectionClaims> {
        let identity = claims.get("sub").and_then(Value::as_str)?;
        let video = claims.get("video")?;
        if video.get("roomJoin").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let room = video.get("room").and_then(Value::as_str)?;
        if identity.is_empty() || identity.len() > 128 || room.is_empty() || room.len() > 128 {
            return None;
        }
        let generation = match claims
            .get("attributes")
            .and_then(|a| a.get(GENERATION_ATTRIBUTE))
        {
            None => None,
            Some(v) => Some(v.as_str()?.parse::<i32>().ok().filter(|g| *g >= 1)?),
        };
        Some(ConnectionClaims {
            resource_name: room.to_string(),
            identity: identity.to_string(),
            generation,
        })
    }

    /// Authenticates a client media credential: edge signature (a LiveKit-signed token is
    /// refused), unexpired, join grant for one room, identity, generation.
    pub fn verify_edge_credential(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<ConnectionClaims, ConnectionDenied> {
        if token.len() > 4096 {
            return Err(ConnectionDenied::InvalidCredential);
        }
        Self::decode_claims(token, EDGE_ISSUER, &self.settings.edge_key, now)
            .as_ref()
            .and_then(Self::connection_claims)
            .ok_or(ConnectionDenied::InvalidCredential)
    }

    /// Re-seals a token LiveKit refreshed for `connection`: verified as LiveKit's, bound to
    /// the same room / identity / generation, then signed with the edge key (LiveKit will
    /// not accept it; only the gateway will, after admission). `None`: refuse it.
    pub fn reseal_refreshed(
        &self,
        token: &str,
        connection: &ConnectionClaims,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let mut claims = Self::decode_claims(
            token,
            &self.settings.api_key,
            &self.settings.api_secret,
            now,
        )?;
        if Self::connection_claims(&claims).as_ref() != Some(connection) {
            return None;
        }
        let object = claims.as_object_mut()?;
        object.insert("iss".into(), Value::String(EDGE_ISSUER.into()));
        let cap = now.timestamp() + MAX_RESEALED_TTL_SECONDS;
        let exp = object.get("exp").and_then(Value::as_i64)?.min(cap);
        object.insert("exp".into(), Value::from(exp));
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.settings.edge_key.expose_secret().as_bytes()),
        )
        .ok()
    }

    /// Server API token with exactly one administrative grant.
    fn admin_token(&self, video: Value) -> Result<String, MediaProviderError> {
        self.sign(
            Signer::LiveKit,
            None,
            Duration::seconds(ADMIN_TOKEN_TTL_SECONDS),
            video,
            BTreeMap::new(),
        )
        .map(|(t, _)| t)
    }

    async fn call(
        &self,
        method: &str,
        grant: Value,
        body: Value,
    ) -> Result<Value, MediaProviderError> {
        self.twirp("RoomService", method, grant, body).await
    }

    /// One server API request (`livekit.{service}/{method}`) under a short-lived token
    /// carrying exactly `grant`.
    pub(crate) async fn twirp(
        &self,
        service: &str,
        method: &str,
        grant: Value,
        body: Value,
    ) -> Result<Value, MediaProviderError> {
        let token = self.admin_token(grant)?;
        let url = format!("{}/twirp/livekit.{service}/{method}", self.settings.api_url);
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let error = if e.is_timeout() {
                    MediaProviderError::Timeout
                } else {
                    MediaProviderError::Unavailable
                };
                tracing::warn!(method, failure = %error, "media provider request failed");
                error
            })?;
        let status = response.status();
        let payload: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(payload);
        }
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let error = match (status.as_u16(), code) {
            (404, _) | (_, "not_found") => MediaProviderError::NotFound,
            (401 | 403, _) | (_, "unauthenticated" | "permission_denied") => {
                tracing::error!(
                    method,
                    status = status.as_u16(),
                    "media provider refused Calling's credentials"
                );
                MediaProviderError::Rejected
            }
            (400 | 409 | 412 | 422, _) => MediaProviderError::Rejected,
            _ => MediaProviderError::Unavailable,
        };
        tracing::warn!(method, status = status.as_u16(), failure = %error, "media provider request refused");
        Err(error)
    }

    fn room_admin(room: &str) -> Value {
        json!({ "roomAdmin": true, "room": room })
    }

    /// Bridges a public-network leg into `room` (Part E §79): a LiveKit SIP participant
    /// `identity` dials the SIP / media edge's internal media peer through the configured
    /// outbound trunk with the single-use media join code. The token carries exactly
    /// the room-admin grant of that room and the SIP call grant.
    pub async fn attach_sip_bridge(
        &self,
        room: &str,
        identity: &str,
        join_code: &str,
    ) -> Result<(), MediaProviderError> {
        let Some(trunk) = self.settings.sip_trunk_id.as_deref() else {
            return Err(MediaProviderError::Unavailable);
        };
        let now = Utc::now();
        let (issuer, secret) = self.key(Signer::LiveKit);
        let claims = AccessClaims {
            iss: issuer,
            sub: None,
            nbf: now.timestamp() - 5,
            exp: (now + Duration::seconds(ADMIN_TOKEN_TTL_SECONDS)).timestamp(),
            video: Self::room_admin(room),
            sip: Some(json!({ "call": true })),
            attributes: BTreeMap::new(),
        };
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.expose_secret().as_bytes()),
        )
        .map_err(|_| MediaProviderError::Rejected)?;
        let response = self
            .http
            .post(format!(
                "{}/twirp/livekit.SIP/CreateSIPParticipant",
                self.settings.api_url
            ))
            .bearer_auth(token)
            .json(&json!({
                "sip_trunk_id": trunk,
                "sip_call_to": join_code,
                "room_name": room,
                "participant_identity": identity,
                "participant_name": "Public network",
                "play_ringtone": false,
            }))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    MediaProviderError::Timeout
                } else {
                    MediaProviderError::Unavailable
                }
            })?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            404 => Err(MediaProviderError::NotFound),
            400..=499 => Err(MediaProviderError::Rejected),
            _ => Err(MediaProviderError::Unavailable),
        }
    }
}

/// LiveKit permission body for server-authorized media permissions. An empty
/// `can_publish_sources` would mean "every source", so publishing is expressed by the
/// explicit source list and `can_publish` is off when the list is empty.
fn permission_body(permissions: MediaPermissions) -> Value {
    let mut sources = Vec::new();
    if permissions.can_publish_audio {
        sources.push("MICROPHONE");
    }
    if permissions.can_publish_video {
        sources.push("CAMERA");
    }
    json!({
        "can_subscribe": permissions.can_subscribe,
        "can_publish": !sources.is_empty(),
        "can_publish_data": permissions.can_publish_data,
        "can_publish_sources": sources,
        "can_update_metadata": false,
        "hidden": false,
        "recorder": false,
    })
}

/// Field lookup accepting the proto name and the lowerCamelCase JSON name.
fn field<'a>(v: &'a Value, snake: &str, camel: &str) -> Option<&'a Value> {
    v.get(snake).or_else(|| v.get(camel))
}

fn int64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn bool_field(v: &Value, snake: &str, camel: &str) -> bool {
    field(v, snake, camel)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn timestamp(seconds: Option<i64>) -> Option<DateTime<Utc>> {
    seconds
        .filter(|s| *s > 0)
        .and_then(|s| Utc.timestamp_opt(s, 0).single())
}

/// Normalizes a LiveKit `ParticipantInfo` (Twirp proto names or webhook camelCase).
/// A server-side recorder (participant kind EGRESS): started only by Calling's own
/// server API credential for an authorized capture (`media::livekit_egress`). It is not a client
/// connection and is neither admitted as a session nor quarantined.
fn is_recorder(v: &Value) -> bool {
    matches!(v.get("kind"), Some(Value::String(k)) if k == "EGRESS")
        || matches!(v.get("kind"), Some(Value::Number(n)) if n.as_i64() == Some(2))
}

fn participant(v: &Value) -> Option<ProviderParticipant> {
    if is_recorder(v) {
        return None;
    }
    let identity = v.get("identity")?.as_str()?.to_string();
    let permission = v.get("permission").cloned().unwrap_or(Value::Null);
    let sources: Vec<String> = field(&permission, "can_publish_sources", "canPublishSources")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| match s {
                    Value::String(s) => Some(s.to_ascii_uppercase()),
                    Value::Number(n) => Some(match n.as_i64() {
                        Some(1) => "CAMERA".to_string(),
                        Some(2) => "MICROPHONE".to_string(),
                        _ => "OTHER".to_string(),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let can_publish = bool_field(&permission, "can_publish", "canPublish");
    // An empty source list with can_publish means every source.
    let publishes =
        |source: &str| can_publish && (sources.is_empty() || sources.iter().any(|s| s == source));
    let permissions = MediaPermissions {
        can_publish_audio: publishes("MICROPHONE"),
        can_publish_video: publishes("CAMERA"),
        can_publish_data: bool_field(&permission, "can_publish_data", "canPublishData"),
        can_subscribe: bool_field(&permission, "can_subscribe", "canSubscribe"),
    };
    let state = v.get("state");
    let active = match state {
        Some(Value::String(s)) => s == "ACTIVE",
        Some(Value::Number(n)) => n.as_i64() == Some(2),
        _ => false,
    };
    let generation = v
        .get("attributes")
        .and_then(|a| a.get(GENERATION_ATTRIBUTE))
        .and_then(Value::as_str)
        .and_then(|g| g.parse::<i32>().ok())
        .filter(|g| *g >= 1);
    let (mut audio, mut video) = (false, false);
    for track in v
        .get("tracks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = track.get("type");
        let is_audio = matches!(kind, Some(Value::String(s)) if s == "AUDIO")
            || matches!(kind, Some(Value::Number(n)) if n.as_i64() == Some(0));
        let is_video = matches!(kind, Some(Value::String(s)) if s == "VIDEO")
            || matches!(kind, Some(Value::Number(n)) if n.as_i64() == Some(1));
        audio |= is_audio;
        video |= is_video;
    }
    Some(ProviderParticipant {
        identity,
        generation,
        permissions,
        active,
        published_audio: audio,
        published_video: video,
        joined_at: timestamp(int64(field(v, "joined_at", "joinedAt"))),
    })
}

#[async_trait]
impl CallMediaProvider for LiveKitAdapter {
    fn adapter_name(&self) -> &'static str {
        ADAPTER_NAME
    }

    fn capabilities(&self) -> MediaProviderCapabilities {
        MediaProviderCapabilities {
            session_control: true,
            credential_issuance: true,
            participant_removal: true,
            permission_update: true,
            participant_listing: true,
            verified_callbacks: true,
            // The self-hosted server API exposes no per-participant transport statistics.
            quality_metrics: false,
        }
    }

    async fn create_or_ensure_session(
        &self,
        session: &MediaSessionRef,
        spec: &MediaSessionSpec,
    ) -> Result<EnsuredMediaSession, MediaProviderError> {
        // CreateRoom is idempotent for an existing name: it returns that room.
        let room = self
            .call(
                "CreateRoom",
                json!({ "roomCreate": true }),
                json!({
                    "name": session.name.as_str(),
                    "empty_timeout": spec.empty_timeout.num_seconds().max(1),
                    "departure_timeout": 20,
                    "max_participants": spec.max_participants,
                }),
            )
            .await?;
        let resource_ref = room
            .get("sid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 128)
            .map(str::to_string);
        Ok(EnsuredMediaSession { resource_ref })
    }

    async fn issue_credentials(
        &self,
        request: &MediaCredentialRequest,
    ) -> Result<IssuedMediaCredential, MediaProviderError> {
        // Join-only and sealed with the edge key: usable only through the gateway.
        let (token, expires_at) = self.sign(
            Signer::Edge,
            Some(&request.identity),
            request.ttl,
            Self::join_only(request.session.name.as_str()),
            Self::generation_attributes(Some(request.generation)),
        )?;
        Ok(IssuedMediaCredential {
            url: self.settings.gateway_url.clone(),
            token: MediaToken::new(token),
            expires_at,
        })
    }

    async fn remove_session(
        &self,
        participant: &MediaParticipantRef,
    ) -> Result<RemovalOutcome, MediaProviderError> {
        let room = participant.session.name.as_str();
        match self
            .call(
                "RemoveParticipant",
                Self::room_admin(room),
                json!({ "room": room, "identity": participant.identity }),
            )
            .await
        {
            Ok(_) => Ok(RemovalOutcome::Removed),
            Err(MediaProviderError::NotFound) => Ok(RemovalOutcome::NotPresent),
            Err(e) => Err(e),
        }
    }

    async fn update_permissions(
        &self,
        participant: &MediaParticipantRef,
        permissions: MediaPermissions,
    ) -> Result<PermissionOutcome, MediaProviderError> {
        let room = participant.session.name.as_str();
        match self
            .call(
                "UpdateParticipant",
                Self::room_admin(room),
                json!({
                    "room": room,
                    "identity": participant.identity,
                    "permission": permission_body(permissions),
                }),
            )
            .await
        {
            Ok(_) => Ok(PermissionOutcome::Applied),
            Err(MediaProviderError::NotFound) => Ok(PermissionOutcome::NotPresent),
            Err(e) => Err(e),
        }
    }

    async fn terminate_session(
        &self,
        session: &MediaSessionRef,
    ) -> Result<TerminationOutcome, MediaProviderError> {
        match self
            .call(
                "DeleteRoom",
                json!({ "roomCreate": true }),
                json!({ "room": session.name.as_str() }),
            )
            .await
        {
            Ok(_) => Ok(TerminationOutcome::Terminated),
            Err(MediaProviderError::NotFound) => Ok(TerminationOutcome::AlreadyAbsent),
            Err(e) => Err(e),
        }
    }

    async fn list_participants(
        &self,
        session: &MediaSessionRef,
    ) -> Result<Vec<ProviderParticipant>, MediaProviderError> {
        let room = session.name.as_str();
        match self
            .call(
                "ListParticipants",
                Self::room_admin(room),
                json!({ "room": room }),
            )
            .await
        {
            Ok(body) => Ok(body
                .get("participants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(participant)
                .collect()),
            Err(MediaProviderError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    async fn reconcile(
        &self,
        session: &MediaSessionRef,
    ) -> Result<ProviderSessionSnapshot, MediaProviderError> {
        let rooms = self
            .call(
                "ListRooms",
                json!({ "roomList": true }),
                json!({ "names": [session.name.as_str()] }),
            )
            .await?;
        let exists = rooms
            .get("rooms")
            .and_then(Value::as_array)
            .is_some_and(|r| {
                r.iter().any(|room| {
                    room.get("name").and_then(Value::as_str) == Some(session.name.as_str())
                })
            });
        let participants = if exists {
            self.list_participants(session).await?
        } else {
            Vec::new()
        };
        Ok(ProviderSessionSnapshot {
            exists,
            participants,
        })
    }

    fn verify_event(
        &self,
        authorization: Option<&str>,
        body: &[u8],
    ) -> Result<VerifiedMediaEvent, EventVerificationError> {
        let header = authorization
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .ok_or(EventVerificationError::MissingSignature)?;
        let token = header.strip_prefix("Bearer ").unwrap_or(header).trim();
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[self.settings.api_key.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss"]);
        validation.validate_nbf = true;
        validation.leeway = WEBHOOK_LEEWAY_SECONDS;
        let claims = jsonwebtoken::decode::<WebhookClaims>(
            token,
            &DecodingKey::from_secret(self.settings.api_secret.expose_secret().as_bytes()),
            &validation,
        )
        .map_err(|_| EventVerificationError::InvalidSignature)?
        .claims;
        let digest = STANDARD.encode(Sha256::digest(body));
        // Constant-time comparison of the body digest the signature covers.
        let matches = claims.sha256.len() == digest.len()
            && claims
                .sha256
                .bytes()
                .zip(digest.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        if !matches {
            return Err(EventVerificationError::InvalidSignature);
        }
        let event: Value =
            serde_json::from_slice(body).map_err(|_| EventVerificationError::Malformed)?;
        let event_id = event
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-')
                    })
            })
            .ok_or(EventVerificationError::Malformed)?
            .to_string();
        let kind = match event.get("event").and_then(Value::as_str) {
            Some("room_started") => MediaEventKind::ResourceStarted,
            Some("room_finished") => MediaEventKind::ResourceFinished,
            Some("participant_joined") => MediaEventKind::ParticipantJoined,
            Some("participant_left") => MediaEventKind::ParticipantLeft,
            Some("participant_connection_aborted") => MediaEventKind::ParticipantConnectionAborted,
            Some("track_published") => MediaEventKind::TrackPublished,
            Some("track_unpublished") => MediaEventKind::TrackUnpublished,
            Some(_) => MediaEventKind::Other,
            None => return Err(EventVerificationError::Malformed),
        };
        // A recorder's own join / leave / tracks are not session evidence.
        let kind = if event.get("participant").is_some_and(is_recorder) {
            MediaEventKind::Other
        } else {
            kind
        };
        let resource_name = event
            .get("room")
            .and_then(|r| r.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(VerifiedMediaEvent {
            event_id,
            kind,
            resource_name,
            participant: event.get("participant").and_then(participant),
            occurred_at: timestamp(int64(field(&event, "created_at", "createdAt"))),
        })
    }
}

/// The public-network bridge port backed by LiveKit SIP (Part E §79): an outbound SIP
/// participant through the configured trunk to the edge's internal media peer.
pub struct LiveKitSipGateway {
    adapter: std::sync::Arc<LiveKitAdapter>,
}

impl LiveKitSipGateway {
    pub fn new(adapter: std::sync::Arc<LiveKitAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait]
impl crate::telephony::media::PublicMediaGateway for LiveKitSipGateway {
    async fn attach(
        &self,
        room: &str,
        identity: &str,
        join_code: &str,
    ) -> Result<(), MediaProviderError> {
        self.adapter
            .attach_sip_bridge(room, identity, join_code)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    const SECRET: &str = "test-livekit-api-secret-0123456789abcdef";
    const EDGE: &str = "test-media-edge-signing-key-0123456789abcdef";

    fn adapter() -> LiveKitAdapter {
        LiveKitAdapter::new(
            LiveKitSettings::from_lookup(&lookup(&[
                ("CALLING_LIVEKIT_URL", "ws://127.0.0.1:7880"),
                ("CALLING_LIVEKIT_API_URL", "http://127.0.0.1:7880"),
                ("CALLING_LIVEKIT_API_KEY", "callingkey"),
                ("CALLING_LIVEKIT_API_SECRET", SECRET),
                (
                    "CALLING_MEDIA_GATEWAY_URL",
                    "ws://127.0.0.1:8090/v1/calling/media",
                ),
                ("CALLING_MEDIA_EDGE_SIGNING_KEY", EDGE),
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn decode_with(token: &str, secret: &str) -> Value {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp"]);
        jsonwebtoken::decode::<Value>(
            token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        )
        .unwrap()
        .claims
    }

    fn decode(token: &str) -> Value {
        decode_with(token, EDGE)
    }

    fn claims(room: &str, gen: Option<i32>) -> ConnectionClaims {
        ConnectionClaims {
            resource_name: room.into(),
            identity: "cfmid_00".into(),
            generation: gen,
        }
    }

    #[test]
    fn configuration_is_required_secure_and_redacted() {
        let base = [
            ("CALLING_LIVEKIT_URL", "wss://media.example.org"),
            ("CALLING_LIVEKIT_API_URL", "https://media.example.org"),
            ("CALLING_LIVEKIT_API_KEY", "callingkey"),
            ("CALLING_LIVEKIT_API_SECRET", SECRET),
            (
                "CALLING_MEDIA_GATEWAY_URL",
                "wss://calling.example.org/v1/calling/media",
            ),
            ("CALLING_MEDIA_EDGE_SIGNING_KEY", EDGE),
        ];
        let settings = LiveKitSettings::from_lookup(&lookup(&base)).unwrap();
        let debug = format!("{settings:?}");
        assert!(!debug.contains(SECRET) && !debug.contains("callingkey") && !debug.contains(EDGE));
        for missing in base.iter().map(|(k, _)| *k) {
            let pairs: Vec<_> = base
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            assert!(matches!(
                LiveKitSettings::from_lookup(&lookup(&pairs)),
                Err(ConfigError::Missing { .. })
            ));
        }
        for (var, bad) in [
            ("CALLING_LIVEKIT_API_SECRET", "tooshort-x"),
            (
                "CALLING_LIVEKIT_API_SECRET",
                "changeme-changeme-changeme-changeme",
            ),
            ("CALLING_LIVEKIT_URL", "ws://media.example.org"),
            ("CALLING_LIVEKIT_API_URL", "http://media.example.org"),
            ("CALLING_LIVEKIT_API_URL", "ftp://media.example.org"),
            ("CALLING_LIVEKIT_API_KEY", "bad key"),
            ("CALLING_LIVEKIT_API_KEY", EDGE_ISSUER),
            (
                "CALLING_MEDIA_GATEWAY_URL",
                "ws://calling.example.org/v1/calling/media",
            ),
            ("CALLING_MEDIA_EDGE_SIGNING_KEY", "tooshort-x"),
            ("CALLING_MEDIA_EDGE_SIGNING_KEY", SECRET),
        ] {
            let pairs: Vec<_> = base
                .iter()
                .map(|(k, v)| if *k == var { (*k, bad) } else { (*k, *v) })
                .collect();
            let err = LiveKitSettings::from_lookup(&lookup(&pairs)).unwrap_err();
            assert!(
                !err.to_string().contains(bad),
                "{var}: error echoes the value"
            );
        }
    }

    #[tokio::test]
    async fn join_tokens_grant_only_joining_one_room() {
        let adapter = adapter();
        let credential = adapter
            .issue_credentials(&MediaCredentialRequest {
                session: MediaSessionRef {
                    name: super::super::provider::MediaResourceName::for_call(
                        crate::calls::ids::CallId::from_uuid(uuid::Uuid::nil()),
                    ),
                },
                identity: "cfmid_00".into(),
                generation: 3,
                ttl: Duration::minutes(5),
            })
            .await
            .unwrap();
        // The client credential is sealed with the edge key: LiveKit's key does not verify
        // it, and it points at the gateway, never at LiveKit.
        assert_eq!(credential.url, "ws://127.0.0.1:8090/v1/calling/media");
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp"]);
        assert!(jsonwebtoken::decode::<Value>(
            credential.token.expose(),
            &DecodingKey::from_secret(SECRET.as_bytes()),
            &validation
        )
        .is_err());
        let claims = decode(credential.token.expose());
        assert_eq!(claims["iss"], EDGE_ISSUER);
        let video = &claims["video"];
        assert_eq!(video["roomJoin"], true);
        assert_eq!(video["room"], "cfc-00000000000000000000000000000000");
        for denied in [
            "canPublish",
            "canSubscribe",
            "canPublishData",
            "canUpdateOwnMetadata",
            "roomAdmin",
            "roomCreate",
            "roomList",
            "roomRecord",
            "ingressAdmin",
            "recorder",
            "agent",
        ] {
            assert_eq!(video[denied], false, "{denied}");
        }
        assert_eq!(claims["sub"], "cfmid_00");
        assert_eq!(claims["attributes"][GENERATION_ATTRIBUTE], "3");
        let raw = credential.token.expose().to_string();
        assert!(!raw.contains(SECRET));
        let payload = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(raw.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(!payload.contains(SECRET));
    }

    #[tokio::test]
    async fn edge_credentials_are_verified_and_upstream_tokens_stay_join_only() {
        let adapter = adapter();
        let room = "cfc-00000000000000000000000000000000";
        let issued = adapter
            .issue_credentials(&MediaCredentialRequest {
                session: MediaSessionRef {
                    name: super::super::provider::MediaResourceName::for_call(
                        crate::calls::ids::CallId::from_uuid(uuid::Uuid::nil()),
                    ),
                },
                identity: "cfmid_00".into(),
                generation: 4,
                ttl: Duration::minutes(5),
            })
            .await
            .unwrap();
        let now = Utc::now();
        assert_eq!(
            adapter.verify_edge_credential(issued.token.expose(), now),
            Ok(claims(room, Some(4)))
        );
        assert_eq!(
            adapter.verify_edge_credential(issued.token.expose(), now + Duration::minutes(6)),
            Err(ConnectionDenied::InvalidCredential),
            "expired"
        );
        // A token LiveKit would accept (LiveKit key) is NOT a client credential.
        let upstream = adapter.upstream_token(&claims(room, Some(4))).unwrap();
        assert_eq!(
            adapter.verify_edge_credential(&upstream, now),
            Err(ConnectionDenied::InvalidCredential)
        );
        let up = decode_with(&upstream, SECRET);
        assert_eq!(up["iss"], "callingkey");
        assert_eq!(up["video"]["canPublish"], false);
        assert_eq!(up["video"]["canSubscribe"], false);
        assert_eq!(up["attributes"][GENERATION_ATTRIBUTE], "4");
        assert!(up["exp"].as_i64().unwrap() - now.timestamp() <= UPSTREAM_TOKEN_TTL_SECONDS);
        // Malformed generations and grants are refused.
        let forged = |video: Value, attributes: Value, key: &str, iss: &str| {
            jsonwebtoken::encode(
                &Header::new(Algorithm::HS256),
                &json!({ "iss": iss, "sub": "cfmid_00", "nbf": now.timestamp() - 1, "exp": now.timestamp() + 60, "video": video, "attributes": attributes }),
                &EncodingKey::from_secret(key.as_bytes()),
            )
            .unwrap()
        };
        let join = json!({ "roomJoin": true, "room": room });
        for (token, what) in [
            (
                forged(
                    join.clone(),
                    json!({ GENERATION_ATTRIBUTE: "zero" }),
                    EDGE,
                    EDGE_ISSUER,
                ),
                "bad generation",
            ),
            (
                forged(
                    join.clone(),
                    json!({ GENERATION_ATTRIBUTE: "0" }),
                    EDGE,
                    EDGE_ISSUER,
                ),
                "generation 0",
            ),
            (
                forged(
                    json!({ "roomJoin": false, "room": room }),
                    json!({}),
                    EDGE,
                    EDGE_ISSUER,
                ),
                "no join grant",
            ),
            (
                forged(join.clone(), json!({}), SECRET, "callingkey"),
                "LiveKit key",
            ),
            (
                forged(join.clone(), json!({}), EDGE, "callingkey"),
                "wrong issuer",
            ),
            ("not-a-token".to_string(), "garbage"),
        ] {
            assert_eq!(
                adapter.verify_edge_credential(&token, now),
                Err(ConnectionDenied::InvalidCredential),
                "{what}"
            );
        }
        assert_eq!(
            adapter.verify_edge_credential(&forged(join, json!({}), EDGE, EDGE_ISSUER), now),
            Ok(claims(room, None)),
            "no generation: authenticated, refused later by admission"
        );
    }

    #[test]
    fn refreshed_livekit_tokens_are_resealed_for_the_same_connection_only() {
        let adapter = adapter();
        let room = "cfc-00000000000000000000000000000000";
        let now = Utc::now();
        let refreshed = |sub: &str, room: &str, gen: &str, key: &str| {
            jsonwebtoken::encode(
                &Header::new(Algorithm::HS256),
                &json!({ "iss": "callingkey", "sub": sub, "nbf": now.timestamp(), "exp": now.timestamp() + 600,
                         "video": { "roomJoin": true, "room": room, "canPublish": true, "canSubscribe": true },
                         "attributes": { GENERATION_ATTRIBUTE: gen } }),
                &EncodingKey::from_secret(key.as_bytes()),
            )
            .unwrap()
        };
        let connection = claims(room, Some(2));
        let sealed = adapter
            .reseal_refreshed(&refreshed("cfmid_00", room, "2", SECRET), &connection, now)
            .unwrap();
        let c = decode(&sealed);
        assert_eq!(c["iss"], EDGE_ISSUER);
        assert_eq!(
            c["video"]["canPublish"], true,
            "the server's grant is preserved"
        );
        assert_eq!(
            adapter.verify_edge_credential(&sealed, now),
            Ok(connection.clone())
        );
        // LiveKit's key no longer verifies it.
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp"]);
        assert!(jsonwebtoken::decode::<Value>(
            &sealed,
            &DecodingKey::from_secret(SECRET.as_bytes()),
            &validation
        )
        .is_err());
        // Another identity / room / generation, or a token not signed by LiveKit: refused.
        for token in [
            refreshed("cfmid_01", room, "2", SECRET),
            refreshed(
                "cfmid_00",
                "cfc-11111111111111111111111111111111",
                "2",
                SECRET,
            ),
            refreshed("cfmid_00", room, "3", SECRET),
            refreshed("cfmid_00", room, "2", EDGE),
        ] {
            assert!(adapter.reseal_refreshed(&token, &connection, now).is_none());
        }
    }

    fn signed(body: &str, secret: &str) -> String {
        let digest = STANDARD.encode(Sha256::digest(body.as_bytes()));
        let now = Utc::now().timestamp();
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &json!({ "iss": "callingkey", "nbf": now - 1, "exp": now + 300, "sha256": digest }),
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn webhooks_are_verified_before_parsing() {
        let adapter = adapter();
        let body = r#"{"event":"participant_joined","id":"EV_abc123","createdAt":"1700000000","room":{"name":"cfc-00000000000000000000000000000000","sid":"RM_x"},"participant":{"sid":"PA_x","identity":"cfmid_ab","state":"ACTIVE","attributes":{"cfm.gen":"2"},"permission":{"canSubscribe":true,"canPublish":true,"canPublishSources":["MICROPHONE"]}}}"#;
        let event = adapter
            .verify_event(Some(&signed(body, SECRET)), body.as_bytes())
            .unwrap();
        assert_eq!(event.kind, MediaEventKind::ParticipantJoined);
        assert_eq!(event.event_id, "EV_abc123");
        let p = event.participant.unwrap();
        assert_eq!(p.generation, Some(2));
        assert!(p.active && p.permissions.can_publish_audio && !p.permissions.can_publish_video);
        // Calling's own server-side recorder (kind EGRESS) is neither a session nor an
        // unknown connection to quarantine; a client identity claiming nothing is.
        let recorder = body.replace(
            r#""identity":"cfmid_ab""#,
            r#""identity":"EG_x","kind":"EGRESS""#,
        );
        let event = adapter
            .verify_event(Some(&signed(&recorder, SECRET)), recorder.as_bytes())
            .unwrap();
        assert_eq!(event.kind, MediaEventKind::Other);
        assert!(event.participant.is_none());
        assert!(participant(&json!({ "identity": "EG_y", "kind": 2 })).is_none());
        assert!(participant(&json!({ "identity": "EG_y", "kind": "STANDARD" })).is_some());
        // Unsigned, wrongly signed, tampered or expired callbacks are refused.
        assert_eq!(
            adapter.verify_event(None, body.as_bytes()).unwrap_err(),
            EventVerificationError::MissingSignature
        );
        assert_eq!(
            adapter
                .verify_event(
                    Some(&signed(body, "another-secret-0123456789abcdefghij")),
                    body.as_bytes()
                )
                .unwrap_err(),
            EventVerificationError::InvalidSignature
        );
        let tampered = body.replace("cfmid_ab", "cfmid_cd");
        assert_eq!(
            adapter
                .verify_event(Some(&signed(body, SECRET)), tampered.as_bytes())
                .unwrap_err(),
            EventVerificationError::InvalidSignature
        );
        let now = Utc::now().timestamp();
        let expired = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &json!({ "iss": "callingkey", "nbf": now - 900, "exp": now - 600,
                     "sha256": STANDARD.encode(Sha256::digest(body.as_bytes())) }),
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .unwrap();
        assert!(adapter
            .verify_event(Some(&expired), body.as_bytes())
            .is_err());
    }

    #[test]
    fn permission_bodies_list_sources_explicitly() {
        let voice = permission_body(MediaPermissions::default_for(
            crate::calls::kinds::CallKind::Voice,
        ));
        assert_eq!(voice["can_publish_sources"], json!(["MICROPHONE"]));
        assert_eq!(voice["can_publish"], true);
        let none = permission_body(MediaPermissions::NONE);
        assert_eq!(none["can_publish"], false);
        assert_eq!(none["can_subscribe"], false);
        let held = permission_body(
            MediaPermissions::default_for(crate::calls::kinds::CallKind::Video).held(),
        );
        assert_eq!(held["can_publish"], false);
        assert_eq!(held["can_subscribe"], true);
    }
}
