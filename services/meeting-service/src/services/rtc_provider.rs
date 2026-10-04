//! RtcProvider — provider-neutral RTC enforcement boundary (spec §A7, §3.3, Part E).
//!
//! Business services decide authorization; the provider only enforces it. Provider
//! room state is never business authority, and provider callbacks can never grant a
//! role, membership or access.
//!
//! [`LiveKitProvider`] implements the full contract against the LiveKit RoomService
//! admin API (Twirp/protobuf over HTTP, every call bounded by a timeout). Recording is
//! part of the interface but gated until Phase 2 Part G.
//!
//! A provider declares what it can enforce through [`RtcProvider::capabilities`].
//! Credential-only providers keep the Parts A–D semantics (enforcement stays
//! `PENDING_PROVIDER_CONTROL`, ENDING never advances to ENDED) — the business state is
//! never weakened when provider control is unavailable.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use livekit_api::{
    access_token::{AccessToken, TokenVerifier, VideoGrants},
    services::{
        room::{CreateRoomOptions, RoomClient, UpdateParticipantOptions},
        ServiceError,
    },
    webhooks::WebhookReceiver,
};
use livekit_protocol as proto;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::config::LiveKitConfig;
use crate::error::{ApiError, ErrorCode};
use crate::models::{conference_sessions, participant_sessions};
use crate::services::meeting_permissions::MediaPermissions;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RtcError {
    #[error("RTC provider unavailable: {0}")]
    Unavailable(String),
    #[error("RTC provider operation not supported: {0}")]
    NotSupported(&'static str),
    #[error("RTC provider rejected the request: {0}")]
    Rejected(String),
    #[error("RTC provider object not found")]
    NotFound,
    #[error("RTC provider authentication failed")]
    Unauthenticated,
}

impl From<RtcError> for ApiError {
    fn from(err: RtcError) -> Self {
        // Provider details are logged, never returned to clients.
        tracing::warn!("rtc provider error: {err}");
        ApiError::domain(
            ErrorCode::RtcProviderUnavailable,
            "The media service is temporarily unavailable",
        )
    }
}

pub type RtcResult<T> = Result<T, RtcError>;

/// What a provider can enforce. Defaults to nothing (credential-only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RtcCapabilities {
    /// Real room create / lookup / terminate.
    pub room_control: bool,
    /// Real participant removal, permission update, mute and listing.
    pub participant_control: bool,
    /// Verifiable provider webhooks.
    pub webhooks: bool,
}

impl RtcCapabilities {
    pub fn full() -> Self {
        Self {
            room_control: true,
            participant_control: true,
            webhooks: true,
        }
    }
}

/// The provider room of one ConferenceSession.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcRoomRef {
    pub conference_id: Uuid,
    pub room_name: String,
}

/// Canonical room name of a conference (one room per conference, never reused).
pub fn room_name_for(conference_id: Uuid) -> String {
    format!("cf-{}", conference_id.simple())
}

impl RtcRoomRef {
    pub fn for_conference(c: &conference_sessions::Model) -> Self {
        Self {
            conference_id: c.id,
            room_name: c
                .rtc_room_name
                .clone()
                .unwrap_or_else(|| room_name_for(c.id)),
        }
    }
}

/// The provider participant of one ParticipantSession (identity is server-generated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcParticipantRef {
    pub conference_id: Uuid,
    pub room_name: String,
    pub participant_session_id: Uuid,
    pub media_identity: String,
}

impl RtcParticipantRef {
    pub fn new(c: &conference_sessions::Model, s: &participant_sessions::Model) -> Self {
        Self {
            conference_id: c.id,
            room_name: RtcRoomRef::for_conference(c).room_name,
            participant_session_id: s.id,
            media_identity: s.media_identity.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomSpec {
    pub max_participants: u32,
    pub empty_timeout_secs: u32,
    pub departure_timeout_secs: u32,
}

impl Default for RoomSpec {
    fn default() -> Self {
        Self {
            max_participants: 0,
            // Business authority ends rooms; the provider timeout is only a safety net.
            empty_timeout_secs: 6 * 60 * 60,
            departure_timeout_secs: 6 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomInfo {
    pub room_name: String,
    pub room_id: Option<String>,
    pub participant_count: u32,
}

#[derive(Debug, Clone)]
pub struct JoinCredentialRequest {
    pub participant: RtcParticipantRef,
    pub display_name: String,
    /// Derived from the permission engine — never from the client.
    pub permissions: MediaPermissions,
    pub ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct JoinCredentials {
    pub provider: &'static str,
    pub url: String,
    pub credential: String,
    pub expires_at: DateTime<Utc>,
}

/// Media track kinds the business layer reasons about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TrackKind {
    Audio,
    Video,
    ScreenShare,
}

impl TrackKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrackKind::Audio => "AUDIO",
            TrackKind::Video => "VIDEO",
            TrackKind::ScreenShare => "SCREEN_SHARE",
        }
    }
}

/// Publish/subscribe grants as the provider currently enforces them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderGrants {
    pub can_publish_audio: bool,
    pub can_publish_video: bool,
    pub can_publish_screen_share: bool,
    pub can_publish_data: bool,
    pub can_subscribe: bool,
}

impl ProviderGrants {
    pub fn from_permissions(p: &MediaPermissions) -> Self {
        Self {
            can_publish_audio: p.audio_publish,
            can_publish_video: p.video_publish,
            can_publish_screen_share: p.screen_share_publish,
            can_publish_data: p.data_publish,
            can_subscribe: p.subscribe,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcPublishedTrack {
    pub track_id: String,
    pub kind: Option<TrackKind>,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcParticipant {
    pub media_identity: String,
    pub connected: bool,
    pub grants: Option<ProviderGrants>,
    pub tracks: Vec<RtcPublishedTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcRoomSnapshot {
    pub room: Option<RoomInfo>,
    pub participants: Vec<RtcParticipant>,
}

/// Provider-neutral, verified provider callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderEventKind {
    RoomStarted,
    RoomFinished,
    ParticipantJoined,
    ParticipantLeft,
    Other,
}

#[derive(Debug, Clone)]
pub struct ProviderEvent {
    /// Provider event id (dedupe key).
    pub id: String,
    pub kind: ProviderEventKind,
    pub event_type: String,
    pub room_name: Option<String>,
    pub room_id: Option<String>,
    pub media_identity: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait RtcProvider: Send + Sync {
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> RtcCapabilities {
        RtcCapabilities::default()
    }

    /// Create or ensure the conference room (idempotent).
    async fn create_room(&self, room: &RtcRoomRef, spec: &RoomSpec) -> RtcResult<RoomInfo>;
    async fn get_room(&self, room: &RtcRoomRef) -> RtcResult<Option<RoomInfo>>;
    /// Terminate the room and disconnect everyone. An already-absent room is success.
    async fn terminate_room(&self, room: &RtcRoomRef, reason: &str) -> RtcResult<()>;
    async fn issue_join_credentials(
        &self,
        request: &JoinCredentialRequest,
    ) -> RtcResult<JoinCredentials>;
    /// Disconnect one participant session. An already-absent participant is success.
    async fn remove_participant(&self, participant: &RtcParticipantRef, reason: &str)
        -> RtcResult<()>;
    /// Replace the participant's live publish/subscribe grants.
    async fn update_permissions(
        &self,
        participant: &RtcParticipantRef,
        permissions: &MediaPermissions,
    ) -> RtcResult<()>;
    /// Server-side mute of every published track of `kind`. Returns tracks muted.
    async fn mute_track(&self, participant: &RtcParticipantRef, kind: TrackKind) -> RtcResult<u32>;
    async fn list_participants(&self, room: &RtcRoomRef) -> RtcResult<Vec<RtcParticipant>>;
    /// Provider rooms whose name starts with `prefix` (reconciliation).
    async fn list_rooms(&self, _prefix: &str) -> RtcResult<Vec<RoomInfo>> {
        Err(RtcError::NotSupported("list_rooms"))
    }
    /// Gated until Phase 2 Part G (recording lifecycle).
    async fn start_recording(&self, room: &RtcRoomRef, profile: &str) -> RtcResult<String>;
    async fn stop_recording(&self, room: &RtcRoomRef) -> RtcResult<()>;
    /// Provider view of one room for drift detection.
    async fn reconcile(&self, room: &RtcRoomRef) -> RtcResult<RtcRoomSnapshot> {
        let found = self.get_room(room).await?;
        let participants = match found {
            Some(_) => self.list_participants(room).await?,
            None => vec![],
        };
        Ok(RtcRoomSnapshot {
            room: found,
            participants,
        })
    }
    /// Verify and parse a provider webhook. Forged/unsigned callbacks are rejected.
    fn parse_webhook(&self, _body: &str, _authorization: Option<&str>) -> RtcResult<ProviderEvent> {
        Err(RtcError::NotSupported("webhooks"))
    }
}

// ---------------------------------------------------------------------------
// LiveKit
// ---------------------------------------------------------------------------

/// LiveKit implementation. Grants are projected from business permissions only.
pub struct LiveKitProvider {
    api_key: String,
    api_secret: String,
    /// Client-facing signalling URL (ws/wss).
    url: String,
    /// RoomService admin endpoint (http/https).
    api_url: String,
    client: RoomClient,
    timeout: Duration,
}

/// `wss://host` → `https://host`, `ws://host` → `http://host`.
pub fn admin_url_from(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}

impl LiveKitProvider {
    pub fn new(config: &LiveKitConfig) -> Self {
        let api_url = std::env::var("LIVEKIT_API_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| admin_url_from(&config.url));
        let timeout_ms = std::env::var("LIVEKIT_API_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000u64);
        Self::with_api_url(config, &api_url, Duration::from_millis(timeout_ms))
    }

    pub fn with_api_url(config: &LiveKitConfig, api_url: &str, timeout: Duration) -> Self {
        Self {
            api_key: config.api_key.clone(),
            api_secret: config.api_secret.clone(),
            url: config.url.clone(),
            api_url: api_url.to_string(),
            client: RoomClient::with_api_key(api_url, &config.api_key, &config.api_secret),
            timeout,
        }
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    fn source_names(p: &MediaPermissions) -> Vec<String> {
        let mut sources = Vec::new();
        if p.audio_publish {
            sources.push("microphone".to_string());
        }
        if p.video_publish {
            sources.push("camera".to_string());
        }
        if p.screen_share_publish {
            sources.push("screen_share".to_string());
            sources.push("screen_share_audio".to_string());
        }
        sources
    }

    pub fn grants_for(room_name: &str, p: &MediaPermissions) -> VideoGrants {
        let sources = Self::source_names(p);
        VideoGrants {
            room_join: true,
            room: room_name.to_string(),
            can_publish: !sources.is_empty(),
            can_publish_sources: sources,
            can_subscribe: p.subscribe,
            can_publish_data: p.data_publish,
            // A client credential is never an admin/recorder credential.
            room_admin: false,
            room_record: false,
            room_create: false,
            room_list: false,
            can_update_own_metadata: false,
            ..VideoGrants::default()
        }
    }

    /// Live permission object pushed to the provider for an existing session.
    pub fn participant_permission(p: &MediaPermissions) -> proto::ParticipantPermission {
        let mut sources = Vec::new();
        if p.audio_publish {
            sources.push(proto::TrackSource::Microphone as i32);
        }
        if p.video_publish {
            sources.push(proto::TrackSource::Camera as i32);
        }
        if p.screen_share_publish {
            sources.push(proto::TrackSource::ScreenShare as i32);
            sources.push(proto::TrackSource::ScreenShareAudio as i32);
        }
        proto::ParticipantPermission {
            can_subscribe: p.subscribe,
            can_publish: !sources.is_empty(),
            can_publish_data: p.data_publish,
            can_publish_sources: sources,
            hidden: false,
            can_update_metadata: false,
            ..Default::default()
        }
    }

    fn grants_from_permission(p: &proto::ParticipantPermission) -> ProviderGrants {
        let has = |s: proto::TrackSource| p.can_publish_sources.contains(&(s as i32));
        // An empty source list with can_publish = true means "every source" in LiveKit.
        let all = p.can_publish && p.can_publish_sources.is_empty();
        ProviderGrants {
            can_publish_audio: p.can_publish && (all || has(proto::TrackSource::Microphone)),
            can_publish_video: p.can_publish && (all || has(proto::TrackSource::Camera)),
            can_publish_screen_share: p.can_publish
                && (all || has(proto::TrackSource::ScreenShare)),
            can_publish_data: p.can_publish_data,
            can_subscribe: p.can_subscribe,
        }
    }

    fn track_kind(t: &proto::TrackInfo) -> Option<TrackKind> {
        match proto::TrackSource::try_from(t.source).ok()? {
            proto::TrackSource::Microphone => Some(TrackKind::Audio),
            proto::TrackSource::Camera => Some(TrackKind::Video),
            proto::TrackSource::ScreenShare | proto::TrackSource::ScreenShareAudio => {
                Some(TrackKind::ScreenShare)
            }
            proto::TrackSource::Unknown => match proto::TrackType::try_from(t.r#type).ok()? {
                proto::TrackType::Audio => Some(TrackKind::Audio),
                proto::TrackType::Video => Some(TrackKind::Video),
                proto::TrackType::Data => None,
            },
        }
    }

    fn participant(p: &proto::ParticipantInfo) -> RtcParticipant {
        RtcParticipant {
            media_identity: p.identity.clone(),
            connected: p.state != proto::participant_info::State::Disconnected as i32,
            grants: p.permission.as_ref().map(Self::grants_from_permission),
            tracks: p
                .tracks
                .iter()
                .map(|t| RtcPublishedTrack {
                    track_id: t.sid.clone(),
                    kind: Self::track_kind(t),
                    muted: t.muted,
                })
                .collect(),
        }
    }

    fn room_info(r: &proto::Room) -> RoomInfo {
        RoomInfo {
            room_name: r.name.clone(),
            room_id: (!r.sid.is_empty()).then(|| r.sid.clone()),
            participant_count: r.num_participants,
        }
    }

    fn map_err(err: ServiceError) -> RtcError {
        let text = err.to_string();
        if text.contains("not_found") {
            RtcError::NotFound
        } else if text.contains("unauthenticated") || text.contains("permission_denied") {
            RtcError::Unauthenticated
        } else if text.contains("invalid_argument") || text.contains("malformed") {
            RtcError::Rejected(text)
        } else {
            RtcError::Unavailable(text)
        }
    }

    async fn bounded<T, F>(&self, f: F) -> RtcResult<T>
    where
        F: std::future::Future<Output = Result<T, ServiceError>>,
    {
        match tokio::time::timeout(self.timeout, f).await {
            Ok(r) => r.map_err(Self::map_err),
            Err(_) => Err(RtcError::Unavailable("provider request timed out".into())),
        }
    }

    fn webhook_receiver(&self) -> WebhookReceiver {
        WebhookReceiver::new(TokenVerifier::with_api_key(&self.api_key, &self.api_secret))
    }
}

const PART_G: &str = "recording lifecycle is delivered in Phase 2 Part G";

#[async_trait]
impl RtcProvider for LiveKitProvider {
    fn name(&self) -> &'static str {
        "LIVEKIT"
    }

    fn capabilities(&self) -> RtcCapabilities {
        RtcCapabilities::full()
    }

    async fn create_room(&self, room: &RtcRoomRef, spec: &RoomSpec) -> RtcResult<RoomInfo> {
        // LiveKit CreateRoom is idempotent: an existing room is returned unchanged.
        let created = self
            .bounded(self.client.create_room(
                &room.room_name,
                CreateRoomOptions {
                    empty_timeout: spec.empty_timeout_secs,
                    departure_timeout: spec.departure_timeout_secs,
                    max_participants: spec.max_participants,
                    ..Default::default()
                },
            ))
            .await?;
        if created.name != room.room_name {
            return Err(RtcError::Rejected("provider returned a different room".into()));
        }
        Ok(Self::room_info(&created))
    }

    async fn get_room(&self, room: &RtcRoomRef) -> RtcResult<Option<RoomInfo>> {
        let rooms = self
            .bounded(self.client.list_rooms(vec![room.room_name.clone()]))
            .await?;
        Ok(rooms
            .iter()
            .find(|r| r.name == room.room_name)
            .map(Self::room_info))
    }

    async fn terminate_room(&self, room: &RtcRoomRef, reason: &str) -> RtcResult<()> {
        tracing::info!(conference_id = %room.conference_id, reason, "terminating provider room");
        match self.bounded(self.client.delete_room(&room.room_name)).await {
            Ok(()) | Err(RtcError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn issue_join_credentials(
        &self,
        request: &JoinCredentialRequest,
    ) -> RtcResult<JoinCredentials> {
        let p = &request.participant;
        let grants = Self::grants_for(&p.room_name, &request.permissions);
        let credential = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity(&p.media_identity)
            .with_name(&request.display_name)
            .with_grants(grants)
            .with_ttl(request.ttl)
            .to_jwt()
            .map_err(|e| RtcError::Unavailable(e.to_string()))?;
        Ok(JoinCredentials {
            provider: "LIVEKIT",
            url: self.url.clone(),
            credential,
            expires_at: Utc::now() + chrono::Duration::from_std(request.ttl).unwrap_or_default(),
        })
    }

    async fn remove_participant(
        &self,
        participant: &RtcParticipantRef,
        reason: &str,
    ) -> RtcResult<()> {
        tracing::info!(
            conference_id = %participant.conference_id,
            participant_session_id = %participant.participant_session_id,
            reason,
            "removing provider participant"
        );
        match self
            .bounded(
                self.client
                    .remove_participant(&participant.room_name, &participant.media_identity),
            )
            .await
        {
            Ok(()) | Err(RtcError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn update_permissions(
        &self,
        participant: &RtcParticipantRef,
        permissions: &MediaPermissions,
    ) -> RtcResult<()> {
        self.bounded(self.client.update_participant(
            &participant.room_name,
            &participant.media_identity,
            UpdateParticipantOptions {
                permission: Some(Self::participant_permission(permissions)),
                ..Default::default()
            },
        ))
        .await
        .map(|_| ())
    }

    async fn mute_track(&self, participant: &RtcParticipantRef, kind: TrackKind) -> RtcResult<u32> {
        let info = self
            .bounded(
                self.client
                    .get_participant(&participant.room_name, &participant.media_identity),
            )
            .await?;
        let mut muted = 0;
        for track in info
            .tracks
            .iter()
            .filter(|t| Self::track_kind(t) == Some(kind))
        {
            if track.muted {
                continue;
            }
            // `mute_published_track` unwraps the response body; isolate it so a malformed
            // provider reply cannot take down the request task.
            let client =
                RoomClient::with_api_key(&self.api_url, &self.api_key, &self.api_secret);
            let (room, identity, sid) = (
                participant.room_name.clone(),
                participant.media_identity.clone(),
                track.sid.clone(),
            );
            let timeout = self.timeout;
            let handle = tokio::spawn(async move {
                tokio::time::timeout(
                    timeout,
                    client.mute_published_track(&room, &identity, &sid, true),
                )
                .await
            });
            match handle.await {
                Ok(Ok(Ok(_))) => muted += 1,
                Ok(Ok(Err(e))) => return Err(Self::map_err(e)),
                Ok(Err(_)) => return Err(RtcError::Unavailable("provider request timed out".into())),
                Err(_) => return Err(RtcError::Rejected("malformed provider mute response".into())),
            }
        }
        Ok(muted)
    }

    async fn list_participants(&self, room: &RtcRoomRef) -> RtcResult<Vec<RtcParticipant>> {
        match self
            .bounded(self.client.list_participants(&room.room_name))
            .await
        {
            Ok(list) => Ok(list.iter().map(Self::participant).collect()),
            Err(RtcError::NotFound) => Ok(vec![]),
            Err(e) => Err(e),
        }
    }

    async fn list_rooms(&self, prefix: &str) -> RtcResult<Vec<RoomInfo>> {
        let rooms = self.bounded(self.client.list_rooms(vec![])).await?;
        Ok(rooms
            .iter()
            .filter(|r| r.name.starts_with(prefix))
            .map(Self::room_info)
            .collect())
    }

    async fn start_recording(&self, _room: &RtcRoomRef, _profile: &str) -> RtcResult<String> {
        Err(RtcError::NotSupported(PART_G))
    }

    async fn stop_recording(&self, _room: &RtcRoomRef) -> RtcResult<()> {
        Err(RtcError::NotSupported(PART_G))
    }

    fn parse_webhook(&self, body: &str, authorization: Option<&str>) -> RtcResult<ProviderEvent> {
        let token = authorization
            .map(|a| a.strip_prefix("Bearer ").unwrap_or(a).trim())
            .filter(|t| !t.is_empty())
            .ok_or(RtcError::Unauthenticated)?;
        let event = self
            .webhook_receiver()
            .receive(body, token)
            .map_err(|_| RtcError::Unauthenticated)?;
        let kind = match event.event.as_str() {
            "room_started" => ProviderEventKind::RoomStarted,
            "room_finished" => ProviderEventKind::RoomFinished,
            "participant_joined" => ProviderEventKind::ParticipantJoined,
            "participant_left" | "participant_connection_aborted" => {
                ProviderEventKind::ParticipantLeft
            }
            _ => ProviderEventKind::Other,
        };
        let id = if event.id.is_empty() {
            let digest = Sha256::digest(body.as_bytes());
            format!(
                "sha256:{}",
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        } else {
            event.id.chars().take(150).collect()
        };
        Ok(ProviderEvent {
            id,
            kind,
            event_type: event.event.chars().take(64).collect(),
            room_name: event.room.as_ref().map(|r| r.name.clone()),
            room_id: event
                .room
                .as_ref()
                .filter(|r| !r.sid.is_empty())
                .map(|r| r.sid.clone()),
            media_identity: event.participant.as_ref().map(|p| p.identity.clone()),
            occurred_at: (event.created_at > 0)
                .then(|| Utc.timestamp_opt(event.created_at, 0).single())
                .flatten(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant_ref() -> RtcParticipantRef {
        RtcParticipantRef {
            conference_id: Uuid::new_v4(),
            room_name: "cf-room".into(),
            participant_session_id: Uuid::new_v4(),
            media_identity: "ps_1".into(),
        }
    }

    #[test]
    fn grants_follow_media_permissions() {
        let viewer = MediaPermissions {
            subscribe: true,
            ..MediaPermissions::none()
        };
        let g = LiveKitProvider::grants_for("room", &viewer);
        assert!(!g.can_publish);
        assert!(g.can_publish_sources.is_empty());
        assert!(g.can_subscribe);
        assert!(!g.room_admin);
        let live = LiveKitProvider::participant_permission(&viewer);
        assert!(!live.can_publish && live.can_publish_sources.is_empty() && live.can_subscribe);
        // Round trip: provider grants derived back from the pushed permission.
        assert_eq!(
            LiveKitProvider::grants_from_permission(&live),
            ProviderGrants::from_permissions(&viewer)
        );

        let participant = MediaPermissions {
            audio_publish: true,
            video_publish: true,
            subscribe: true,
            data_publish: true,
            ..MediaPermissions::none()
        };
        let g = LiveKitProvider::grants_for("room", &participant);
        assert!(g.can_publish);
        assert_eq!(g.can_publish_sources, vec!["microphone", "camera"]);
        let live = LiveKitProvider::participant_permission(&participant);
        assert_eq!(
            LiveKitProvider::grants_from_permission(&live),
            ProviderGrants::from_permissions(&participant)
        );
    }

    #[test]
    fn admin_url_mapping() {
        assert_eq!(admin_url_from("wss://lk.example"), "https://lk.example");
        assert_eq!(admin_url_from("ws://127.0.0.1:7880"), "http://127.0.0.1:7880");
        assert_eq!(admin_url_from("https://x"), "https://x");
    }

    #[tokio::test]
    async fn livekit_issues_signed_short_lived_credentials() {
        let config = LiveKitConfig {
            url: "wss://lk.test".into(),
            api_key: "key".into(),
            api_secret: "secret-secret-secret-secret-secret".into(),
        };
        let provider = LiveKitProvider::with_api_url(
            &config,
            "http://127.0.0.1:1",
            Duration::from_millis(300),
        );
        let creds = provider
            .issue_join_credentials(&JoinCredentialRequest {
                participant: participant_ref(),
                display_name: "A".into(),
                permissions: MediaPermissions::none(),
                ttl: Duration::from_secs(60),
            })
            .await
            .unwrap();
        let claims = TokenVerifier::with_api_key("key", "secret-secret-secret-secret-secret")
            .verify(&creds.credential)
            .unwrap();
        assert_eq!(claims.sub, "ps_1");
        assert_eq!(claims.video.room, "cf-room");
        assert!(!claims.video.can_publish);
        assert!(!claims.video.room_admin);
        // Unreachable provider → bounded, stable Unavailable (never a fake success).
        let room = RtcRoomRef {
            conference_id: Uuid::new_v4(),
            room_name: "cf-room".into(),
        };
        assert!(matches!(
            provider.create_room(&room, &RoomSpec::default()).await,
            Err(RtcError::Unavailable(_))
        ));
        assert!(matches!(
            provider.start_recording(&room, "default").await,
            Err(RtcError::NotSupported(_))
        ));
    }

    #[test]
    fn forged_webhooks_are_rejected() {
        let config = LiveKitConfig {
            url: "wss://lk.test".into(),
            api_key: "key".into(),
            api_secret: "secret-secret-secret-secret-secret".into(),
        };
        let provider = LiveKitProvider::with_api_url(&config, "http://x", Duration::from_secs(1));
        let body = r#"{"event":"participant_joined","id":"EV_1","room":{"name":"cf-x"}}"#;
        assert_eq!(
            provider.parse_webhook(body, None).unwrap_err(),
            RtcError::Unauthenticated
        );
        // Signed with another secret.
        let forged = AccessToken::with_api_key("key", "another-secret-another-secret-xx")
            .with_sha256("abc")
            .to_jwt()
            .unwrap();
        assert!(provider.parse_webhook(body, Some(&forged)).is_err());
        // Correct key but body hash mismatch.
        let digest = Sha256::digest(b"different body");
        use base64::Engine;
        let sha = base64::engine::general_purpose::STANDARD.encode(digest);
        let wrong_body = AccessToken::with_api_key("key", "secret-secret-secret-secret-secret")
            .with_sha256(&sha)
            .to_jwt()
            .unwrap();
        assert!(provider.parse_webhook(body, Some(&wrong_body)).is_err());
        // Correctly signed.
        let sha = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(body.as_bytes()));
        let good = AccessToken::with_api_key("key", "secret-secret-secret-secret-secret")
            .with_sha256(&sha)
            .to_jwt()
            .unwrap();
        let e = provider.parse_webhook(body, Some(&good)).unwrap();
        assert_eq!(e.kind, ProviderEventKind::ParticipantJoined);
        assert_eq!(e.id, "EV_1");
        assert_eq!(e.room_name.as_deref(), Some("cf-x"));
    }
}
