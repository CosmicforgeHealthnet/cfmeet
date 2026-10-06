//! Provider-neutral transcription, translation and intelligence boundaries (Part G
//! §29–§35). No meeting business service calls a speech / AI vendor directly; every
//! call goes through these traits after the policy, feature-flag and consent gates.
//!
//! Each provider declares where it processes data (`ExternalAllowed` = third-party
//! service, `SelfHostedOnly` = inside the deployment) so organization policy
//! (`EXTERNAL_ALLOWED` / `SELF_HOSTED_ONLY` / `DISABLED`) can be enforced before any
//! dispatch. Unconfigured providers report no capability and never process anything.

use async_trait::async_trait;
use uuid::Uuid;

use crate::services::profiles::ProcessingLocation;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider rejected the request: {0}")]
    Rejected(String),
    #[error("provider job not found")]
    NotFound,
    #[error("provider not configured")]
    NotConfigured,
}

pub type ProviderResult<T> = Result<T, ProviderError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptionMode {
    Live,
    PostMeeting,
}

impl TranscriptionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptionMode::Live => "LIVE",
            TranscriptionMode::PostMeeting => "POST_MEETING",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptionCapabilities {
    pub live: bool,
    pub post_meeting: bool,
    /// Where audio / text is processed (`None` = not configured).
    pub location: Option<ProcessingLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderJobStatus {
    Pending,
    Active,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderJob {
    pub provider_job_ref: String,
    pub status: ProviderJobStatus,
}

#[derive(Debug, Clone)]
pub struct LiveTranscriptionRequest {
    pub job_id: Uuid,
    pub conference_id: Uuid,
    /// Provider room to listen to (server-chosen).
    pub room_name: String,
    pub language: String,
}

#[derive(Debug, Clone)]
pub struct PostMeetingTranscriptionRequest {
    pub job_id: Uuid,
    pub conference_id: Uuid,
    /// Short-lived signed read URL of the source recording (never stored).
    pub source_url: String,
    pub language: String,
}

#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> TranscriptionCapabilities {
        TranscriptionCapabilities::default()
    }
    async fn start_live_transcription(
        &self,
        request: &LiveTranscriptionRequest,
    ) -> ProviderResult<ProviderJob>;
    async fn stop_live_transcription(&self, provider_job_ref: &str) -> ProviderResult<ProviderJob>;
    async fn submit_post_meeting_transcription(
        &self,
        request: &PostMeetingTranscriptionRequest,
    ) -> ProviderResult<ProviderJob>;
    async fn get_job(&self, provider_job_ref: &str) -> ProviderResult<Option<ProviderJob>>;
    async fn cancel_job(&self, provider_job_ref: &str) -> ProviderResult<()>;
}

pub struct UnconfiguredTranscriptionProvider;

#[async_trait]
impl TranscriptionProvider for UnconfiguredTranscriptionProvider {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn start_live_transcription(
        &self,
        _: &LiveTranscriptionRequest,
    ) -> ProviderResult<ProviderJob> {
        Err(ProviderError::NotConfigured)
    }
    async fn stop_live_transcription(&self, _: &str) -> ProviderResult<ProviderJob> {
        Err(ProviderError::NotConfigured)
    }
    async fn submit_post_meeting_transcription(
        &self,
        _: &PostMeetingTranscriptionRequest,
    ) -> ProviderResult<ProviderJob> {
        Err(ProviderError::NotConfigured)
    }
    async fn get_job(&self, _: &str) -> ProviderResult<Option<ProviderJob>> {
        Err(ProviderError::NotConfigured)
    }
    async fn cancel_job(&self, _: &str) -> ProviderResult<()> {
        Err(ProviderError::NotConfigured)
    }
}

/// Translation is downstream of transcription and never overwrites the original.
#[async_trait]
pub trait TranslationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn location(&self) -> Option<ProcessingLocation> {
        None
    }
    async fn translate(
        &self,
        text: &str,
        source_language: &str,
        target_language: &str,
    ) -> ProviderResult<String>;
}

pub struct UnconfiguredTranslationProvider;

#[async_trait]
impl TranslationProvider for UnconfiguredTranslationProvider {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn translate(&self, _: &str, _: &str, _: &str) -> ProviderResult<String> {
        Err(ProviderError::NotConfigured)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntelligenceCapabilities {
    pub summarize: bool,
    pub action_items: bool,
    pub location: Option<ProcessingLocation>,
}

/// Input built from an authorized, READY source artefact. Speakers are opaque labels
/// (never contact details).
#[derive(Debug, Clone)]
pub struct IntelligenceInput {
    pub conference_id: Uuid,
    pub language: String,
    pub lines: Vec<(String, String)>,
}

#[async_trait]
pub trait IntelligenceProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> IntelligenceCapabilities {
        IntelligenceCapabilities::default()
    }
    async fn summarize(&self, input: &IntelligenceInput) -> ProviderResult<String>;
    async fn extract_action_items(&self, input: &IntelligenceInput) -> ProviderResult<Vec<String>>;
}

/// Production default: AI processing is not configured (and disabled by flag).
pub struct UnconfiguredIntelligenceProvider;

#[async_trait]
impl IntelligenceProvider for UnconfiguredIntelligenceProvider {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn summarize(&self, _: &IntelligenceInput) -> ProviderResult<String> {
        Err(ProviderError::NotConfigured)
    }
    async fn extract_action_items(&self, _: &IntelligenceInput) -> ProviderResult<Vec<String>> {
        Err(ProviderError::NotConfigured)
    }
}

/// Policy check shared by transcription / translation / intelligence dispatch:
/// a provider may run only where policy permits.
pub fn location_permitted(
    policy: ProcessingLocation,
    provider: Option<ProcessingLocation>,
) -> bool {
    match (policy, provider) {
        (ProcessingLocation::Disabled, _) | (_, None) | (_, Some(ProcessingLocation::Disabled)) => {
            false
        }
        (ProcessingLocation::SelfHostedOnly, Some(p)) => p == ProcessingLocation::SelfHostedOnly,
        (ProcessingLocation::ExternalAllowed, Some(_)) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProcessingLocation::*;

    #[test]
    fn provider_location_must_fit_policy() {
        assert!(!location_permitted(Disabled, Some(SelfHostedOnly)));
        assert!(!location_permitted(SelfHostedOnly, Some(ExternalAllowed)));
        assert!(location_permitted(SelfHostedOnly, Some(SelfHostedOnly)));
        assert!(location_permitted(ExternalAllowed, Some(ExternalAllowed)));
        assert!(location_permitted(ExternalAllowed, Some(SelfHostedOnly)));
        assert!(!location_permitted(ExternalAllowed, None));
    }
}
