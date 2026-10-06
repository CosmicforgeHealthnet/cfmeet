//! RecordingProvider — provider-neutral recording / egress boundary (Part G §8).
//!
//! Recording (egress) has materially different semantics from room/participant control
//! (separate provider service, its own job ids, its own lifecycle callbacks), so it is a
//! dedicated internal provider rather than more methods on `RtcProvider` — whose Part E
//! `start_recording` / `stop_recording` stubs are superseded and never called by
//! business code. Business handlers never call LiveKit Egress directly.
//!
//! Provider job ids are internal (`meeting_artifacts.provider_ref`) and never projected.
//! A provider status is evidence, never authorization.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use livekit_api::services::egress::{
    encoding::EncodingOptions, EgressClient, EgressListFilter, EgressListOptions, EgressOutput,
    RoomCompositeOptions,
};
use livekit_protocol as proto;

use crate::config::{operations::RecordingEncoding, LiveKitConfig};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordingError {
    #[error("recording provider unavailable: {0}")]
    Unavailable(String),
    #[error("recording provider rejected the request: {0}")]
    Rejected(String),
    #[error("recording job not found")]
    NotFound,
    #[error("recording is not supported by this provider")]
    NotSupported,
}

pub type RecordingResult<T> = Result<T, RecordingError>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordingCapabilities {
    /// Real composite (whole meeting) capture.
    pub composite: bool,
    /// Lifecycle callbacks through the verified provider webhook.
    pub webhooks: bool,
    /// Job query / listing for reconciliation.
    pub query: bool,
}

/// Provider-neutral recording job status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRecordingStatus {
    Starting,
    Active,
    Ending,
    Complete,
    Failed,
    Aborted,
    LimitReached,
}

impl ProviderRecordingStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Failed | Self::Aborted | Self::LimitReached
        )
    }
}

/// Output the provider reports for a finished capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOutput {
    /// Object key the provider wrote (relative to the provider's storage config).
    pub filename: String,
    pub size_bytes: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecording {
    pub provider_ref: String,
    pub room_name: String,
    pub status: ProviderRecordingStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub output: Option<ProviderOutput>,
    /// Internal diagnostics only (never returned to clients).
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompositeRecordingRequest {
    pub room_name: String,
    /// `grid` / `speaker`.
    pub layout: String,
    /// Object key the capture must be written to (server-chosen).
    pub output_key: String,
}

#[async_trait]
pub trait RecordingProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> RecordingCapabilities {
        RecordingCapabilities::default()
    }
    async fn start_composite(
        &self,
        request: &CompositeRecordingRequest,
    ) -> RecordingResult<ProviderRecording>;
    async fn stop(&self, provider_ref: &str) -> RecordingResult<ProviderRecording>;
    async fn query(&self, provider_ref: &str) -> RecordingResult<Option<ProviderRecording>>;
    /// Active captures of a provider room (provider-side idempotency + drift).
    async fn list_active(&self, room_name: &str) -> RecordingResult<Vec<ProviderRecording>>;
}

/// No recording provider configured: capture is unavailable.
pub struct UnconfiguredRecorder;

#[async_trait]
impl RecordingProvider for UnconfiguredRecorder {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn start_composite(
        &self,
        _: &CompositeRecordingRequest,
    ) -> RecordingResult<ProviderRecording> {
        Err(RecordingError::NotSupported)
    }
    async fn stop(&self, _: &str) -> RecordingResult<ProviderRecording> {
        Err(RecordingError::NotSupported)
    }
    async fn query(&self, _: &str) -> RecordingResult<Option<ProviderRecording>> {
        Err(RecordingError::NotSupported)
    }
    async fn list_active(&self, _: &str) -> RecordingResult<Vec<ProviderRecording>> {
        Err(RecordingError::NotSupported)
    }
}

fn ns_to_time(ns: i64) -> Option<DateTime<Utc>> {
    (ns > 0).then(|| Utc.timestamp_nanos(ns))
}

/// Map a LiveKit `EgressInfo` (API response or webhook payload).
pub fn from_egress_info(info: &proto::EgressInfo) -> ProviderRecording {
    let status = match proto::EgressStatus::try_from(info.status) {
        Ok(proto::EgressStatus::EgressStarting) | Err(_) => ProviderRecordingStatus::Starting,
        Ok(proto::EgressStatus::EgressActive) => ProviderRecordingStatus::Active,
        Ok(proto::EgressStatus::EgressEnding) => ProviderRecordingStatus::Ending,
        Ok(proto::EgressStatus::EgressComplete) => ProviderRecordingStatus::Complete,
        Ok(proto::EgressStatus::EgressFailed) => ProviderRecordingStatus::Failed,
        Ok(proto::EgressStatus::EgressAborted) => ProviderRecordingStatus::Aborted,
        Ok(proto::EgressStatus::EgressLimitReached) => ProviderRecordingStatus::LimitReached,
    };
    let output = info
        .file_results
        .iter()
        .find(|f| !f.filename.is_empty())
        .map(|f| ProviderOutput {
            filename: f.filename.trim_start_matches('/').to_string(),
            size_bytes: (f.size > 0).then_some(f.size as u64),
            duration_ms: (f.duration > 0).then_some((f.duration / 1_000_000) as u64),
        });
    ProviderRecording {
        provider_ref: info.egress_id.clone(),
        room_name: info.room_name.clone(),
        status,
        started_at: ns_to_time(info.started_at),
        ended_at: ns_to_time(info.ended_at),
        output,
        error: (!info.error.is_empty()).then(|| info.error.clone()),
    }
}

/// LiveKit Egress (room composite → file in the egress-configured object storage).
pub struct LiveKitEgressRecorder {
    client: EgressClient,
    timeout: Duration,
    encoding: RecordingEncoding,
}

impl LiveKitEgressRecorder {
    pub fn new(config: &LiveKitConfig, encoding: RecordingEncoding) -> Self {
        let api_url = std::env::var("LIVEKIT_API_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| crate::services::rtc_provider::admin_url_from(&config.url));
        let timeout_ms = std::env::var("LIVEKIT_API_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000u64);
        Self::with_api_url(
            config,
            &api_url,
            Duration::from_millis(timeout_ms),
            encoding,
        )
    }

    pub fn with_api_url(
        config: &LiveKitConfig,
        api_url: &str,
        timeout: Duration,
        encoding: RecordingEncoding,
    ) -> Self {
        Self {
            client: EgressClient::with_api_key(api_url, &config.api_key, &config.api_secret),
            timeout,
            encoding,
        }
    }

    async fn bounded<T, F>(&self, f: F) -> RecordingResult<T>
    where
        F: std::future::Future<Output = Result<T, livekit_api::services::ServiceError>>,
    {
        match tokio::time::timeout(self.timeout, f).await {
            Err(_) => Err(RecordingError::Unavailable("timeout".into())),
            Ok(Ok(v)) => Ok(v),
            Ok(Err(livekit_api::services::ServiceError::Twirp(e))) => {
                let msg = e.to_string();
                if msg.contains("not_found") || msg.contains("not found") {
                    Err(RecordingError::NotFound)
                } else if msg.contains("failed_precondition") || msg.contains("invalid_argument") {
                    Err(RecordingError::Rejected(msg))
                } else {
                    Err(RecordingError::Unavailable(msg))
                }
            }
            Ok(Err(e)) => Err(RecordingError::Unavailable(e.to_string())),
        }
    }

    fn encoding(&self) -> EncodingOptions {
        let (audio_codec, audio_bitrate) = match self.encoding {
            RecordingEncoding::H264Opus => (proto::AudioCodec::Opus, 96),
            RecordingEncoding::ProviderDefault => (proto::AudioCodec::Aac, 128),
        };
        EncodingOptions {
            width: 1280,
            height: 720,
            depth: 24,
            framerate: 30,
            audio_codec,
            audio_bitrate,
            audio_frequency: 48000,
            video_codec: proto::VideoCodec::H264Main,
            video_bitrate: 3000,
            keyframe_interval: 4.0,
            audio_quality: 0,
            video_quality: 0,
        }
    }
}

#[async_trait]
impl RecordingProvider for LiveKitEgressRecorder {
    fn name(&self) -> &'static str {
        "livekit-egress"
    }

    fn capabilities(&self) -> RecordingCapabilities {
        RecordingCapabilities {
            composite: true,
            webhooks: true,
            query: true,
        }
    }

    async fn start_composite(
        &self,
        request: &CompositeRecordingRequest,
    ) -> RecordingResult<ProviderRecording> {
        let output = proto::EncodedFileOutput {
            file_type: proto::EncodedFileType::Mp4 as i32,
            filepath: request.output_key.clone(),
            disable_manifest: true,
            output: None,
        };
        let info = self
            .bounded(self.client.start_room_composite_egress(
                &request.room_name,
                vec![EgressOutput::File(output)],
                RoomCompositeOptions {
                    layout: request.layout.clone(),
                    encoding: self.encoding(),
                    audio_only: false,
                    video_only: false,
                    custom_base_url: String::new(),
                },
            ))
            .await?;
        Ok(from_egress_info(&info))
    }

    async fn stop(&self, provider_ref: &str) -> RecordingResult<ProviderRecording> {
        match self.bounded(self.client.stop_egress(provider_ref)).await {
            Ok(info) => Ok(from_egress_info(&info)),
            // Already finished: report the provider's current view instead.
            Err(RecordingError::Rejected(_)) => match self.query(provider_ref).await? {
                Some(r) => Ok(r),
                None => Err(RecordingError::NotFound),
            },
            Err(e) => Err(e),
        }
    }

    async fn query(&self, provider_ref: &str) -> RecordingResult<Option<ProviderRecording>> {
        let items = self
            .bounded(self.client.list_egress(EgressListOptions {
                filter: EgressListFilter::Egress(provider_ref.to_string()),
                active: false,
            }))
            .await?;
        Ok(items
            .iter()
            .find(|i| i.egress_id == provider_ref)
            .map(from_egress_info))
    }

    async fn list_active(&self, room_name: &str) -> RecordingResult<Vec<ProviderRecording>> {
        let items = self
            .bounded(self.client.list_egress(EgressListOptions {
                filter: EgressListFilter::Room(room_name.to_string()),
                active: true,
            }))
            .await?;
        Ok(items.iter().map(from_egress_info).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_info_maps_to_provider_neutral_recording() {
        let info = proto::EgressInfo {
            egress_id: "EG_1".into(),
            room_name: "cf-x".into(),
            status: proto::EgressStatus::EgressComplete as i32,
            started_at: 1_700_000_000_000_000_000,
            ended_at: 1_700_000_010_000_000_000,
            file_results: vec![proto::FileInfo {
                filename: "/recordings/a.mp4".into(),
                duration: 9_500_000_000,
                size: 1234,
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = from_egress_info(&info);
        assert_eq!(r.status, ProviderRecordingStatus::Complete);
        assert!(r.status.is_terminal());
        let out = r.output.unwrap();
        assert_eq!(out.filename, "recordings/a.mp4");
        assert_eq!(out.size_bytes, Some(1234));
        assert_eq!(out.duration_ms, Some(9500));
        assert!(r.started_at.unwrap() < r.ended_at.unwrap());
        let failed = from_egress_info(&proto::EgressInfo {
            status: proto::EgressStatus::EgressFailed as i32,
            error: "pipeline".into(),
            ..Default::default()
        });
        assert_eq!(failed.status, ProviderRecordingStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("pipeline"));
    }
}
