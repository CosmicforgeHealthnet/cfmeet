//! RtcProvider — provider-neutral RTC enforcement boundary (spec §A7, §3.3).
//!
//! Business services decide authorization; the provider only enforces it. Provider
//! room state is never business authority.
//!
//! Parts A–D status: the trait is complete, and [`LiveKitProvider`] implements
//! credential issuance (role-projected grants). Room lifecycle control, permission
//! updates, removal, recording and reconciliation are Part E and return
//! [`RtcError::NotSupported`]; business services record those intents in the outbox.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use livekit_api::access_token::{AccessToken, VideoGrants};

use crate::config::LiveKitConfig;
use crate::error::{ApiError, ErrorCode};
use crate::services::meeting_permissions::MediaPermissions;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RtcError {
    #[error("RTC provider unavailable: {0}")]
    Unavailable(String),
    #[error("RTC provider operation not supported yet: {0}")]
    NotSupported(&'static str),
    #[error("RTC provider rejected the request: {0}")]
    Rejected(String),
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

#[derive(Debug, Clone)]
pub struct RoomSpec {
    pub room_name: String,
    pub max_participants: u32,
    pub empty_timeout_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomInfo {
    pub room_name: String,
    pub room_id: Option<String>,
    pub participant_count: u32,
}

#[derive(Debug, Clone)]
pub struct JoinCredentialRequest {
    pub room_name: String,
    /// Server-generated, unique within the conference.
    pub media_identity: String,
    pub display_name: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcParticipant {
    pub media_identity: String,
    pub connected: bool,
}

#[async_trait]
pub trait RtcProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn create_room(&self, spec: &RoomSpec) -> RtcResult<RoomInfo>;
    async fn get_room(&self, room_name: &str) -> RtcResult<Option<RoomInfo>>;
    async fn terminate_room(&self, room_name: &str) -> RtcResult<()>;
    async fn issue_join_credentials(
        &self,
        request: &JoinCredentialRequest,
    ) -> RtcResult<JoinCredentials>;
    async fn remove_participant(&self, room_name: &str, media_identity: &str) -> RtcResult<()>;
    async fn update_permissions(
        &self,
        room_name: &str,
        media_identity: &str,
        permissions: &MediaPermissions,
    ) -> RtcResult<()>;
    async fn mute_track(
        &self,
        room_name: &str,
        media_identity: &str,
        track_id: &str,
        muted: bool,
    ) -> RtcResult<()>;
    async fn list_participants(&self, room_name: &str) -> RtcResult<Vec<RtcParticipant>>;
    async fn start_recording(&self, room_name: &str) -> RtcResult<String>;
    async fn stop_recording(&self, room_name: &str, recording_id: &str) -> RtcResult<()>;
    async fn reconcile(&self, room_name: &str) -> RtcResult<Option<RoomInfo>>;
}

/// LiveKit implementation. Wraps the same signing material as the legacy
/// `LiveKitService`, but grants are projected from business permissions.
pub struct LiveKitProvider {
    api_key: String,
    api_secret: String,
    url: String,
}

impl LiveKitProvider {
    pub fn new(config: &LiveKitConfig) -> Self {
        Self {
            api_key: config.api_key.clone(),
            api_secret: config.api_secret.clone(),
            url: config.url.clone(),
        }
    }

    pub fn grants_for(room_name: &str, p: &MediaPermissions) -> VideoGrants {
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
        VideoGrants {
            room_join: true,
            room: room_name.to_string(),
            can_publish: !sources.is_empty(),
            can_publish_sources: sources,
            can_subscribe: p.subscribe,
            can_publish_data: p.data_publish,
            room_admin: false,
            room_record: false,
            ..VideoGrants::default()
        }
    }
}

const PART_E: &str = "provider control plane is delivered in Phase 2 Part E";

#[async_trait]
impl RtcProvider for LiveKitProvider {
    fn name(&self) -> &'static str {
        "LIVEKIT"
    }

    async fn create_room(&self, _spec: &RoomSpec) -> RtcResult<RoomInfo> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn get_room(&self, _room_name: &str) -> RtcResult<Option<RoomInfo>> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn terminate_room(&self, _room_name: &str) -> RtcResult<()> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn issue_join_credentials(
        &self,
        request: &JoinCredentialRequest,
    ) -> RtcResult<JoinCredentials> {
        let grants = Self::grants_for(&request.room_name, &request.permissions);
        let credential = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity(&request.media_identity)
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

    async fn remove_participant(&self, _room_name: &str, _media_identity: &str) -> RtcResult<()> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn update_permissions(&self, _r: &str, _m: &str, _p: &MediaPermissions) -> RtcResult<()> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn mute_track(&self, _r: &str, _m: &str, _t: &str, _muted: bool) -> RtcResult<()> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn list_participants(&self, _room_name: &str) -> RtcResult<Vec<RtcParticipant>> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn start_recording(&self, _room_name: &str) -> RtcResult<String> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn stop_recording(&self, _room_name: &str, _recording_id: &str) -> RtcResult<()> {
        Err(RtcError::NotSupported(PART_E))
    }

    async fn reconcile(&self, _room_name: &str) -> RtcResult<Option<RoomInfo>> {
        Err(RtcError::NotSupported(PART_E))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[tokio::test]
    async fn livekit_issues_signed_short_lived_credentials() {
        let provider = LiveKitProvider::new(&LiveKitConfig {
            url: "wss://lk.test".into(),
            api_key: "key".into(),
            api_secret: "secret-secret-secret-secret-secret".into(),
        });
        let creds = provider
            .issue_join_credentials(&JoinCredentialRequest {
                room_name: "cf-room".into(),
                media_identity: "ps_1".into(),
                display_name: "A".into(),
                permissions: MediaPermissions::none(),
                ttl: Duration::from_secs(60),
            })
            .await
            .unwrap();
        let claims = livekit_api::access_token::TokenVerifier::with_api_key(
            "key",
            "secret-secret-secret-secret-secret",
        )
        .verify(&creds.credential)
        .unwrap();
        assert_eq!(claims.sub, "ps_1");
        assert_eq!(claims.video.room, "cf-room");
        assert!(!claims.video.can_publish);
        assert!(matches!(
            provider.terminate_room("cf-room").await,
            Err(RtcError::NotSupported(_))
        ));
    }
}
