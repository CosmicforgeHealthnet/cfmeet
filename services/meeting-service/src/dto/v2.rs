//! `/api/v2` meeting-domain contract (camelCase, bare JSON on success).
//!
//! Request DTOs use `deny_unknown_fields`: identity, ownership, role or admission
//! state can never be smuggled in through a body (e.g. `userId`, `ownerUserId`,
//! `role: "HOST"` on join) — such requests are rejected outright.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{
        AccessPolicy, AdmissionState, BanScope, ConferenceKind, ConferenceStatus, IdentityType,
        InvitationStatus, MediaCapturePolicy, MeetingType, MemberStatus, OrganizationMemberStatus,
        OrganizationRole, OrganizationStatus, PrincipalType, RecurrenceExceptionKind,
        RecurrenceStatus, Role, SecurityProfile, SpaceStatus, TerminationReason, WaitingRoomPolicy,
    },
    services::meeting_permissions::{MediaPermissions, RolePolicy},
};

// ---------------------------------------------------------------------------
// Meeting spaces
// ---------------------------------------------------------------------------

/// Access-policy configuration (no secrets).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessConfig {
    /// DOMAIN_RESTRICTED / EXTERNAL_ONLY: verified email domains treated as internal.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// CUSTOM: registered server-side access adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_ref: Option<String>,
    /// HEALTHCARE_APPOINTMENT: optional appointment reference forwarded to the
    /// appointment authority (which alone decides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appointment_ref: Option<String>,
    /// Fail closed when an authenticated session's revocation cannot be verified
    /// (always on for HEALTHCARE and HIGH_SECURITY security profiles).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_verified_revocation: bool,
    /// CUSTOM waiting-room evaluator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_room_adapter_ref: Option<String>,
}

/// Start-window / late-start / auto-end policy (minutes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchedulePolicy {
    #[serde(default = "SchedulePolicy::default_early")]
    pub early_start_minutes: u32,
    #[serde(default = "SchedulePolicy::default_late")]
    pub late_start_grace_minutes: u32,
    #[serde(default = "SchedulePolicy::default_auto_end")]
    pub auto_end: bool,
    #[serde(default = "SchedulePolicy::default_auto_end_grace")]
    pub auto_end_grace_minutes: u32,
}

impl SchedulePolicy {
    fn default_early() -> u32 {
        15
    }
    fn default_late() -> u32 {
        60
    }
    fn default_auto_end() -> bool {
        true
    }
    fn default_auto_end_grace() -> u32 {
        15
    }
}

impl Default for SchedulePolicy {
    fn default() -> Self {
        Self {
            early_start_minutes: Self::default_early(),
            late_start_grace_minutes: Self::default_late(),
            auto_end: Self::default_auto_end(),
            auto_end_grace_minutes: Self::default_auto_end_grace(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMeetingSpaceRequest {
    pub title: String,
    pub description: Option<String>,
    pub meeting_type: Option<MeetingType>,
    /// IANA timezone, e.g. `Africa/Lagos`. Defaults to `UTC`.
    pub timezone: Option<String>,
    pub access_policy: Option<AccessPolicy>,
    pub waiting_room_policy: Option<WaitingRoomPolicy>,
    pub security_profile: Option<SecurityProfile>,
    /// PRESENTER, PARTICIPANT (default) or VIEWER.
    pub default_role: Option<Role>,
    /// Plaintext passcode; only a bcrypt hash is stored and it is never returned.
    pub passcode: Option<String>,
    pub interactive_capacity: Option<i32>,
    pub viewer_capacity: Option<i32>,
    pub recording_policy: Option<MediaCapturePolicy>,
    pub transcription_policy: Option<MediaCapturePolicy>,
    pub retention_policy_ref: Option<String>,
    /// Must be an organization the caller verifiably belongs to.
    pub organization_id: Option<Uuid>,
    pub access_config: Option<AccessConfig>,
    pub role_policy: Option<RolePolicy>,
    pub schedule_policy: Option<SchedulePolicy>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMeetingSpaceRequest {
    /// Optimistic concurrency version the client last read.
    pub version: i32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub meeting_type: Option<MeetingType>,
    pub timezone: Option<String>,
    pub access_policy: Option<AccessPolicy>,
    pub waiting_room_policy: Option<WaitingRoomPolicy>,
    pub security_profile: Option<SecurityProfile>,
    pub default_role: Option<Role>,
    pub passcode: Option<String>,
    #[serde(default)]
    pub clear_passcode: bool,
    pub interactive_capacity: Option<i32>,
    pub viewer_capacity: Option<i32>,
    pub recording_policy: Option<MediaCapturePolicy>,
    pub transcription_policy: Option<MediaCapturePolicy>,
    pub retention_policy_ref: Option<String>,
    pub access_config: Option<AccessConfig>,
    pub role_policy: Option<RolePolicy>,
    pub schedule_policy: Option<SchedulePolicy>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionedRequest {
    pub version: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DuplicateMeetingSpaceRequest {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerTransferRequest {
    pub new_owner_user_id: Uuid,
    pub version: i32,
    /// Keep the previous owner as a HOST member (default true).
    #[serde(default = "default_true")]
    pub retain_previous_owner: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstantMeetingRequest {
    pub title: Option<String>,
    pub timezone: Option<String>,
    pub access_policy: Option<AccessPolicy>,
    pub waiting_room_policy: Option<WaitingRoomPolicy>,
    pub passcode: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResourceView {
    /// Full configuration (owner / host members).
    Full,
    /// Minimal data for any caller holding the link/code.
    Summary,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSpaceResponse {
    pub id: Uuid,
    pub meeting_code: String,
    pub view: ResourceView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub meeting_type: MeetingType,
    pub status: SpaceStatus,
    pub access_policy: AccessPolicy,
    pub requires_passcode: bool,
    pub is_legacy: bool,
    /// The caller's persistent role in this space, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer_role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_room_policy: Option<WaitingRoomPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_profile: Option<SecurityProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_capacity: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer_capacity: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_policy: Option<MediaCapturePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcription_policy: Option<MediaCapturePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_policy_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_user_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_config: Option<AccessConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_policy: Option<RolePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_policy: Option<SchedulePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSpaceListResponse {
    pub items: Vec<MeetingSpaceResponse>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstantMeetingResponse {
    pub meeting_space: MeetingSpaceResponse,
    pub conference: ConferenceResponse,
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMemberRequest {
    /// Registered CF Meet user. Exactly one of `userId` / `email`.
    pub user_id: Option<Uuid>,
    /// Matched only against verified account emails.
    pub email: Option<String>,
    pub role: Role,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateMemberRequest {
    pub role: Role,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberResponse {
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub principal_type: PrincipalType,
    pub user_id: Option<Uuid>,
    pub email: Option<String>,
    pub role: Role,
    pub status: MemberStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberListResponse {
    pub items: Vec<MemberResponse>,
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateInvitationRequest {
    /// Role granted on use (default PARTICIPANT); bounded by the inviter's grant authority.
    pub role: Option<Role>,
    /// Restrict the invitation to one occurrence.
    pub conference_id: Option<Uuid>,
    pub invitee_email: Option<String>,
    /// Default 168 (7 days); capped by server policy.
    pub expires_in_hours: Option<i64>,
    pub use_limit: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InvitationResponse {
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub requested_role: Role,
    pub invitee_email: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub use_limit: Option<i32>,
    pub usage_count: i32,
    pub status: InvitationStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvitationResponse {
    pub invitation: InvitationResponse,
    /// Raw bearer token. Returned exactly once; only its hash is stored.
    pub token: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveInvitationResponse {
    pub meeting_space: MeetingSpaceResponse,
    pub conference_id: Option<Uuid>,
    pub requested_role: Role,
    pub expires_at: DateTime<Utc>,
    pub remaining_uses: Option<i32>,
}

// ---------------------------------------------------------------------------
// Conferences & recurrence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateConferenceRequest {
    pub scheduled_start_at: Option<DateTime<Utc>>,
    pub scheduled_end_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RescheduleConferenceRequest {
    pub scheduled_start_at: DateTime<Utc>,
    pub scheduled_end_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListConferencesQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceResponse {
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub status: ConferenceStatus,
    pub view: ResourceView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ConferenceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_start_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_end_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_requested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<TerminationReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_locked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_exception: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_start_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_legacy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceListResponse {
    pub items: Vec<ConferenceResponse>,
}

/// Provider enforcement state for operations whose media effect is Part E.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProviderEnforcement {
    /// Business state committed; provider intent queued in the outbox for the RTC control plane.
    PendingProviderControl,
    /// The RTC provider confirmed the enforcement (room ready / terminated, media disconnected).
    Enforced,
    NotRequired,
}

impl From<crate::services::rtc_control::Enforcement> for ProviderEnforcement {
    fn from(e: crate::services::rtc_control::Enforcement) -> Self {
        use crate::services::rtc_control::Enforcement;
        match e {
            Enforcement::Enforced => ProviderEnforcement::Enforced,
            Enforcement::Pending => ProviderEnforcement::PendingProviderControl,
            Enforcement::NotRequired => ProviderEnforcement::NotRequired,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceActionResponse {
    pub conference: ConferenceResponse,
    /// False when the request was an idempotent no-op (e.g. already LIVE).
    pub changed: bool,
    pub provider_enforcement: ProviderEnforcement,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetRecurrenceRequest {
    /// RFC 5545 RRULE body, e.g. `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10`.
    pub rrule: String,
    /// First occurrence, wall-clock time in the meeting space timezone.
    #[schema(value_type = String, example = "2026-10-05T09:00:00")]
    pub start_local: NaiveDateTime,
    pub duration_minutes: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecurrenceResponse {
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub rrule: String,
    pub timezone: String,
    #[schema(value_type = String)]
    pub start_local: NaiveDateTime,
    pub duration_minutes: i32,
    pub status: RecurrenceStatus,
    pub materialized_until: Option<DateTime<Utc>>,
    pub version: i32,
}

// ---------------------------------------------------------------------------
// Join / participants
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinConferenceRequest {
    /// Presentation name only — never identity. Required for guests.
    pub display_name: Option<String>,
    pub passcode: Option<String>,
    pub invitation_token: Option<String>,
    /// Reconnect: supersede this earlier session of the same participant.
    pub previous_session_id: Option<Uuid>,
    /// Opaque context forwarded to a server-side access authority (e.g. a HealthNet
    /// appointment token). Never treated as proof by the meeting service itself.
    pub external_context: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JoinConferenceSummary {
    pub id: Uuid,
    pub status: ConferenceStatus,
    pub meeting_space_id: Uuid,
    pub is_locked: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantResponse {
    pub id: Uuid,
    pub conference_id: Uuid,
    pub identity_type: IdentityType,
    pub display_name: String,
    pub role: Role,
    pub admission_state: AdmissionState,
    pub requested_at: DateTime<Utc>,
    pub admitted_at: Option<DateTime<Utc>>,
    /// User-visible denial message (internal notes are never exposed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason_code: Option<String>,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantSessionResponse {
    pub id: Uuid,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RtcCredentialResponse {
    pub provider: String,
    pub url: String,
    /// Short-lived provider credential.
    pub credential: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RtcStatus {
    Issued,
    /// Participant is waiting: no media credential of any kind is issued.
    NotIssuedWaiting,
    /// Business admission committed; provider credential issuance is gated (Part E flag).
    ProviderControlDisabled,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JoinPermissions {
    pub audio_publish: bool,
    pub video_publish: bool,
    pub screen_share_publish: bool,
    pub data_publish: bool,
    pub subscribe: bool,
}

impl From<MediaPermissions> for JoinPermissions {
    fn from(p: MediaPermissions) -> Self {
        Self {
            audio_publish: p.audio_publish,
            video_publish: p.video_publish,
            screen_share_publish: p.screen_share_publish,
            data_publish: p.data_publish,
            subscribe: p.subscribe,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JoinPolicy {
    pub recording_active: bool,
    pub transcription_active: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JoinConferenceResponse {
    pub conference: JoinConferenceSummary,
    pub participant: ParticipantResponse,
    pub participant_session: Option<ParticipantSessionResponse>,
    /// Server-issued participant credential for non-registered actors (send as
    /// `X-Participant-Token`). Returned only when newly issued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_token: Option<String>,
    pub rtc: Option<RtcCredentialResponse>,
    pub rtc_status: RtcStatus,
    pub permissions: JoinPermissions,
    pub policy: JoinPolicy,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaveConferenceRequest {
    /// Close one session; omitted = close all of the caller's sessions.
    pub participant_session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveConferenceResponse {
    pub closed_sessions: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WaitingRoomResponse {
    pub conference_id: Uuid,
    pub participants: Vec<ParticipantResponse>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmitRequest {
    #[serde(default)]
    pub participant_ids: Vec<Uuid>,
    /// Admit every participant currently WAITING.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DenyRequest {
    #[serde(default)]
    pub participant_ids: Vec<Uuid>,
    #[serde(default)]
    pub all: bool,
    pub reason_code: Option<String>,
    /// Shown to the denied participant.
    pub message: Option<String>,
    /// Administrative note — stored, never shown to the participant.
    pub internal_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedParticipant {
    pub participant_id: Uuid,
    pub admission_state: AdmissionState,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkAdmissionResponse {
    /// Transitioned by this request.
    pub changed: Vec<Uuid>,
    /// Already in the requested state (idempotent retry).
    pub unchanged: Vec<Uuid>,
    /// In a state that does not allow this transition.
    pub skipped: Vec<SkippedParticipant>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleChangeRequest {
    pub role: Role,
    /// Optional optimistic version of the participant.
    pub version: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveParticipantRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BanParticipantRequest {
    /// CONFERENCE (default) or SPACE (all future occurrences).
    pub scope: Option<BanScope>,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModerationResponse {
    pub participant: ParticipantResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ban_id: Option<Uuid>,
    pub provider_enforcement: ProviderEnforcement,
}

// ---------------------------------------------------------------------------
// Compatibility
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveResponse {
    pub meeting_space: MeetingSpaceResponse,
    /// The canonical current/next conference, if any.
    pub conference: Option<ConferenceResponse>,
    /// How the identifier matched: V2_ID, V2_CODE, LEGACY_ID or LEGACY_CODE.
    pub matched_by: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShadowComparisonResponse {
    pub legacy_meeting_id: Uuid,
    pub meeting_space_id: Option<Uuid>,
    pub consistent: bool,
    pub differences: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackfillReportResponse {
    pub legacy_meeting_rows: i64,
    pub meeting_space_rows: i64,
    pub mapped_spaces: i64,
    pub mapping_failures: i64,
    pub legacy_conference_sessions: i64,
    pub legacy_participant_rows: i64,
    pub mapped_legacy_participant_rows: i64,
    pub conference_participant_rows_from_legacy: i64,
    pub participant_session_rows_from_legacy: i64,
    pub unmapped_meeting_ids: Vec<Uuid>,
    pub unmapped_participant_ids: Vec<Uuid>,
    pub inserted_spaces: u64,
    pub inserted_conferences: u64,
    pub inserted_participants: u64,
    pub inserted_sessions: u64,
}

// ---------------------------------------------------------------------------
// Organizations (authoritative ORGANIZATION_ONLY membership)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateOrganizationRequest {
    pub name: String,
    /// Lowercase letters, digits and '-'.
    pub slug: String,
    pub owner_user_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateOrganizationRequest {
    pub status: OrganizationStatus,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpsertOrganizationMemberRequest {
    pub role: OrganizationRole,
    pub status: OrganizationMemberStatus,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: OrganizationStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationMemberResponse {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub role: OrganizationRole,
    pub status: OrganizationMemberStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyOrganizationResponse {
    pub organization: OrganizationResponse,
    pub role: OrganizationRole,
}

// ---------------------------------------------------------------------------
// Recurrence exceptions (independent of materialization)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecurrenceExceptionAction {
    Cancel,
    Reschedule,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecurrenceExceptionRequest {
    /// The occurrence's original start (as generated by the series), any distance ahead.
    pub original_start_at: DateTime<Utc>,
    pub action: RecurrenceExceptionAction,
    /// RESCHEDULE only.
    pub scheduled_start_at: Option<DateTime<Utc>>,
    /// RESCHEDULE only; defaults to the series duration.
    pub scheduled_end_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecurrenceExceptionResponse {
    pub id: Uuid,
    pub recurrence_rule_id: Uuid,
    /// Original wall-clock start in the series timezone (exception key).
    #[schema(value_type = String, example = "2027-03-29T09:00:00")]
    pub original_start_local: NaiveDateTime,
    pub timezone: String,
    pub kind: RecurrenceExceptionKind,
    pub new_start_at: Option<DateTime<Utc>>,
    pub new_end_at: Option<DateTime<Utc>>,
    pub version: i32,
    /// Set when the occurrence was already materialized and has been updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_to_conference_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Part E — media control & realtime
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MuteRequest {
    pub track_kind: crate::services::rtc_provider::TrackKind,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MuteResponse {
    pub participant_id: Uuid,
    pub track_kind: crate::services::rtc_provider::TrackKind,
    /// Connected device sessions examined.
    pub sessions: u32,
    /// Published tracks the provider muted.
    pub muted_tracks: u32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeTicketResponse {
    /// One-shot, short-lived ticket for `GET …/realtime?ticket=` (browser WebSockets).
    pub ticket: String,
    pub expires_at: DateTime<Utc>,
    /// WebSocket path relative to the API origin.
    pub websocket_path: String,
    /// Current conference event sequence (start cursor for a fresh subscriber).
    pub cursor: i64,
    /// `FULL` (admitted / members) or `SELF_ONLY` (waiting participants).
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize, Default, ToSchema)]
pub struct EventReplayQuery {
    /// Last sequence the client processed.
    pub after: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventReplayResponse {
    pub events: Vec<crate::services::realtime::RealtimeEnvelope>,
    /// Cursor to resume from.
    pub cursor: i64,
    /// True when the cursor cannot be served: refetch `canonical` and resume from `cursor`.
    pub resync_required: bool,
    pub canonical: String,
}
