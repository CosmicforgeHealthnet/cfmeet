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
//! Credentials (§42–§44):
//!
//! * join tokens grant `roomJoin` for exactly the Call's canonical room and the session's
//!   media identity, and NOTHING else — no publish, no subscribe, no data, no metadata
//!   updates, no admin / create / list / record / ingress / agent grant. The session's
//!   media generation is carried as the participant attribute `cfm.gen`; a client cannot
//!   change attributes (no `canUpdateOwnMetadata`);
//! * the server grants the effective media permissions with `UpdateParticipant` only after
//!   it verified the connection (webhook / listing) against the database;
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

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MIN_SECRET_LEN: usize = 32;
/// Lifetime of the server's own API tokens.
const ADMIN_TOKEN_TTL_SECONDS: i64 = 60;
/// Accepted clock skew for webhook tokens.
const WEBHOOK_LEEWAY_SECONDS: u64 = 30;

/// Calling's LiveKit configuration. No `Debug` derive: the secret is redacted by hand.
pub struct LiveKitSettings {
    /// URL clients connect to (`wss://…`, `ws://` only for loopback development).
    client_url: String,
    /// Server API base URL (`https://…`, `http://` only for loopback development).
    api_url: String,
    api_key: String,
    api_secret: SecretString,
    api_timeout: StdDuration,
}

impl fmt::Debug for LiveKitSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveKitSettings")
            .field("client_url", &self.client_url)
            .field("api_url", &self.api_url)
            .field("api_key", &"[REDACTED]")
            .field("api_secret", &"[REDACTED]")
            .field("api_timeout", &self.api_timeout)
            .finish()
    }
}

impl LiveKitSettings {
    /// A copy for building an adapter (the secret stays in a redacted container).
    pub(crate) fn clone_settings(&self) -> Self {
        Self {
            client_url: self.client_url.clone(),
            api_url: self.api_url.clone(),
            api_key: self.api_key.clone(),
            api_secret: self.api_secret.clone(),
            api_timeout: self.api_timeout,
        }
    }

    /// Reads `CALLING_LIVEKIT_URL`, `CALLING_LIVEKIT_API_URL`, `CALLING_LIVEKIT_API_KEY`,
    /// `CALLING_LIVEKIT_API_SECRET` and `CALLING_LIVEKIT_API_TIMEOUT_MS`. Every value is
    /// required (no fallback credential exists); invalid values are refused without echoing
    /// them.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let required = |var: &'static str| {
            get(var)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or(ConfigError::Missing { var })
        };
        let client_url = required("CALLING_LIVEKIT_URL")?;
        check_url("CALLING_LIVEKIT_URL", &client_url, &["wss://"], &["ws://"])?;
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
        Ok(Self {
            client_url,
            api_url,
            api_key,
            api_secret: SecretString::from(api_secret),
            api_timeout: StdDuration::from_millis(api_timeout),
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    attributes: BTreeMap<&'a str, String>,
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

    fn sign(
        &self,
        identity: Option<&str>,
        ttl: Duration,
        video: Value,
        attributes: BTreeMap<&str, String>,
    ) -> Result<(String, DateTime<Utc>), MediaProviderError> {
        let now = Utc::now();
        let expires_at = now + ttl;
        let claims = AccessClaims {
            iss: &self.settings.api_key,
            sub: identity,
            // LiveKit validates nbf strictly; allow for small clock skew between hosts.
            nbf: now.timestamp() - 5,
            exp: expires_at.timestamp(),
            video,
            attributes,
        };
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.settings.api_secret.expose_secret().as_bytes()),
        )
        .map_err(|_| MediaProviderError::Rejected)?;
        Ok((token, expires_at))
    }

    /// Server API token with exactly one administrative grant.
    fn admin_token(&self, video: Value) -> Result<String, MediaProviderError> {
        self.sign(
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
        let token = self.admin_token(grant)?;
        let url = format!(
            "{}/twirp/livekit.RoomService/{method}",
            self.settings.api_url
        );
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
fn participant(v: &Value) -> Option<ProviderParticipant> {
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
        // Join-only: every capability is explicitly false (LiveKit treats an absent
        // publish / subscribe / data grant as TRUE).
        let video = json!({
            "room": request.session.name.as_str(),
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
        });
        let mut attributes = BTreeMap::new();
        attributes.insert(GENERATION_ATTRIBUTE, request.generation.to_string());
        let (token, expires_at) =
            self.sign(Some(&request.identity), request.ttl, video, attributes)?;
        Ok(IssuedMediaCredential {
            url: self.settings.client_url.clone(),
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

    fn adapter() -> LiveKitAdapter {
        LiveKitAdapter::new(
            LiveKitSettings::from_lookup(&lookup(&[
                ("CALLING_LIVEKIT_URL", "ws://127.0.0.1:7880"),
                ("CALLING_LIVEKIT_API_URL", "http://127.0.0.1:7880"),
                ("CALLING_LIVEKIT_API_KEY", "callingkey"),
                ("CALLING_LIVEKIT_API_SECRET", SECRET),
            ]))
            .unwrap(),
        )
        .unwrap()
    }

    fn decode(token: &str) -> Value {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp"]);
        jsonwebtoken::decode::<Value>(
            token,
            &DecodingKey::from_secret(SECRET.as_bytes()),
            &validation,
        )
        .unwrap()
        .claims
    }

    #[test]
    fn configuration_is_required_secure_and_redacted() {
        let base = [
            ("CALLING_LIVEKIT_URL", "wss://media.example.org"),
            ("CALLING_LIVEKIT_API_URL", "https://media.example.org"),
            ("CALLING_LIVEKIT_API_KEY", "callingkey"),
            ("CALLING_LIVEKIT_API_SECRET", SECRET),
        ];
        let settings = LiveKitSettings::from_lookup(&lookup(&base)).unwrap();
        let debug = format!("{settings:?}");
        assert!(!debug.contains(SECRET) && !debug.contains("callingkey"));
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
        let claims = decode(credential.token.expose());
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
