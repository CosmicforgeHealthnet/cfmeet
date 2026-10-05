//! Parts G–I `/api/v2` DTOs (recording & artefacts, specialized profiles, quality).
//! Request bodies reject unknown fields and never carry authority: no user / actor /
//! participant ids for the caller, no provider recording ids, no storage buckets or
//! keys, no roles, no consent actor. Domain types live next to their services and are
//! re-exported here.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub use crate::services::artifacts::{
    consent::{
        ConsentDecision, ConsentDecisionInput, ConsentRequirement, ConsentRequirementsResponse,
        RecordConsentRequest, WithdrawalEffect, WithdrawalResponse,
    },
    intelligence::{IntelligenceKind, IntelligenceRequest},
    recording::{ProviderDispatch, RecordingResponse, StartRecordingRequest},
    service::{ArtifactAccess, GenerateReportRequest},
    transcription::{
        ProviderTranscriptionEvent, SegmentView, StartTranscriptionRequest, TranscriptView,
        TranscriptionJobView, TranscriptionModeDto, TranslateRequest, TranslationResponse,
    },
    ArtifactStatus, ArtifactType, ArtifactView,
};
pub use crate::services::profiles::{
    AccessProfile, AuditProfile, ConsentMode, CustomRegistrationField, DataSharingProfile,
    ExportProfile, FieldRequirement, IntelligenceProfile, MeetingProfile, ProcessingLocation,
    ProfilePolicy, RecordingProfile, RegistrationProfile, ResolvedProfile, RoleMappingProfile,
    StageProfile, TranscriptionProfile, WithdrawalMode,
};
pub use crate::services::quality::{
    health::HealthReason,
    operations::{ConferenceHealthSummary, IncidentView},
    service::{
        ConferenceQualityView, FailureCategoryDto, FailureKind, MediaFailure, MediaHealth,
        NetworkType, QualityIngestResponse, QualitySampleBatch, QualitySampleInput,
    },
};
pub use crate::services::specialized::{
    BackstageResponse, ProfilePolicyResponse, ProfilePolicyUpdateRequest, RegistrationCreated,
    RegistrationRequest, RegistrationView,
};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecordingListResponse {
    pub recordings: Vec<ArtifactView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactListResponse {
    pub artifacts: Vec<ArtifactView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionJobListResponse {
    pub jobs: Vec<TranscriptionJobView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationListResponse {
    pub registrations: Vec<RegistrationView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceHealthListResponse {
    pub conferences: Vec<ConferenceHealthSummary>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IncidentListResponse {
    pub incidents: Vec<IncidentView>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ArtifactListQuery {
    /// Filter by artefact type.
    pub artifact_type: Option<ArtifactType>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TranscriptQuery {
    /// Return segments with `sequence > after`.
    pub after: Option<i64>,
    /// Page size (max 1000).
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct OperationsQuery {
    /// HEALTHY / DEGRADED (includes UNHEALTHY) / UNHEALTHY / UNKNOWN.
    pub state: Option<String>,
    /// OPEN / RESOLVED / CLOSED (incidents).
    pub status: Option<String>,
    pub limit: Option<u64>,
}

/// Internal callback acknowledgement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallbackAck {
    pub received: bool,
    pub outcome: String,
    pub job_id: Option<Uuid>,
}
