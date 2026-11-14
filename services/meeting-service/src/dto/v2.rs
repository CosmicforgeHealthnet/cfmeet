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
    services::notifications::EmailDelivery,
    services::{realtime::RealtimeEnvelope, rtc_provider::TrackKind},
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
    /// Part F: sparse collaboration policy defaults for this space.
    pub collaboration_policy: Option<CollaborationPolicyPatch>,
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
    /// Part F: replaces the space's collaboration policy overrides.
    pub collaboration_policy: Option<CollaborationPolicyPatch>,
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
    /// Part F: space-level collaboration overrides (full view only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collaboration_policy: Option<CollaborationPolicyPatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
    /// The canonical conference for this space (live > upcoming > latest).
    /// Useful for showing the current meeting status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_conference: Option<ConferenceSummary>,
}

/// Minimal conference info included in space responses.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceSummary {
    pub id: Uuid,
    pub status: ConferenceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_start_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_end_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSpaceListResponse {
    pub items: Vec<MeetingSpaceResponse>,
    /// Present only on paginated requests (`limit` or `cursor` sent) when another page
    /// exists: pass it back as `cursor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `GET /api/v2/meeting-spaces` paging (both optional; omitting both keeps the legacy
/// complete list).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListMeetingSpacesQuery {
    /// Opaque cursor from a previous page's `nextCursor`.
    pub cursor: Option<String>,
    /// Page size, 1-200 (default 50 when `cursor` is sent without `limit`).
    pub limit: Option<u32>,
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
    /// Blueprint Section 11: also email the invitation link to `inviteeEmail` (required
    /// with this flag). The result is reported in `emailDelivery`. Default false.
    pub send_email: Option<bool>,
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
    /// Only when `sendEmail` was true: DISABLED (notification email is off on this
    /// deployment), ACCEPTED (queued, subject to the recipient's notification preferences)
    /// or FAILED (temporary failure; share the link yourself).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_delivery: Option<EmailDelivery>,
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
    /// New start; more than 5 minutes in the past is rejected.
    pub scheduled_start_at: DateTime<Utc>,
    /// New end. Omitted: the occurrence keeps its duration (a start-only occurrence stays
    /// start-only). At most 24 hours after the start.
    pub scheduled_end_at: Option<DateTime<Utc>>,
    /// Optional optimistic-concurrency guard: the conference `version` last read.
    pub version: Option<i32>,
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
    /// Part G: recording indicator (`OFF` / `REQUESTED` / `RECORDING` / `STOPPING`) —
    /// the authoritative recovery source after a reconnect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_state: Option<String>,
    /// Part G: live transcription indicator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcription_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceListResponse {
    pub items: Vec<ConferenceResponse>,
}

// ---------------------------------------------------------------------------
// Agenda / first-party calendar (Blueprint Section 10)
// ---------------------------------------------------------------------------

/// `GET /api/v2/me/conferences` query.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgendaQuery {
    /// Inclusive lower bound on the scheduled start (RFC 3339). Default: now.
    pub from: Option<DateTime<Utc>>,
    /// Exclusive upper bound (RFC 3339). Default: `from` + 31 days; at most 366 days after `from`.
    pub to: Option<DateTime<Utc>>,
    /// Comma-separated conference statuses, e.g. `CREATED,LIVE`.
    pub status: Option<String>,
    /// Comma-separated relations of the caller (`AgendaRelation`), e.g. `OWNER,INVITEE`.
    pub role: Option<String>,
    /// Restrict to one organization's meetings (the caller must be a verified member).
    pub organization_id: Option<Uuid>,
    /// Opaque cursor from a previous page's `nextCursor`.
    pub cursor: Option<String>,
    /// Page size, 1-200 (default 50).
    pub limit: Option<u32>,
}

/// Why an occurrence is on the caller's agenda.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgendaRelation {
    /// The caller (or the calling environment) owns the meeting space.
    Owner,
    /// Active member of the meeting space.
    Member,
    /// Holder of an active invitation bound to the caller's verified email.
    Invitee,
    /// Registered for the webinar.
    Registrant,
    /// Took part in (or is waiting for) this conference.
    Participant,
    /// Organization meeting visible to a verified member of `organizationId`.
    Organization,
}

impl AgendaRelation {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "OWNER" => Self::Owner,
            "MEMBER" => Self::Member,
            "INVITEE" => Self::Invitee,
            "REGISTRANT" => Self::Registrant,
            "PARTICIPANT" => Self::Participant,
            "ORGANIZATION" => Self::Organization,
            _ => return None,
        })
    }
}

/// Meeting-space fields an agenda entry may show.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgendaSpace {
    /// Omitted for HEALTHCARE / HIGH_SECURITY spaces unless the caller manages the space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub meeting_code: String,
    pub meeting_type: MeetingType,
    pub status: SpaceStatus,
    /// IANA timezone of the meeting: show times in this zone and in the viewer's zone.
    pub timezone: String,
    pub access_policy: AccessPolicy,
    pub requires_passcode: bool,
    /// FULL view only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<Uuid>,
}

/// One occurrence on the caller's agenda.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgendaItem {
    /// The conference id; null for a recurring occurrence beyond the materialization
    /// horizon (it becomes a conference later, under the same `occurrenceKey`).
    pub conference_id: Option<Uuid>,
    /// Stable occurrence identity within the meeting space (unchanged by materialization).
    pub occurrence_key: String,
    pub meeting_space_id: Uuid,
    /// False for an occurrence computed from the recurrence rule (not yet materialized).
    pub materialized: bool,
    pub view: ResourceView,
    pub status: ConferenceStatus,
    pub kind: ConferenceKind,
    pub scheduled_start_at: DateTime<Utc>,
    pub scheduled_end_at: Option<DateTime<Utc>>,
    pub is_exception: bool,
    /// FULL view only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_start_at: Option<DateTime<Utc>>,
    /// FULL view only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// FULL view only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    /// The caller's persistent role in the space, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer_role: Option<Role>,
    pub relations: Vec<AgendaRelation>,
    pub meeting_space: AgendaSpace,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgendaResponse {
    /// Ordered by scheduled start, then meeting space, then occurrence key.
    pub items: Vec<AgendaItem>,
    /// Pass back as `cursor` for the next page; null on the last page.
    pub next_cursor: Option<String>,
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
    /// Part F: explicit consents granted before admission (see the pre-join contract).
    pub consents: Option<Vec<ConsentGrant>>,
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
    /// Part F: the media context (main room or breakout) the credential targets.
    pub media_context: MediaContextView,
    /// Part J: how to follow admission / denial and recover state. A WAITING
    /// participant subscribes here (scope SELF_ONLY) instead of receiving media.
    pub realtime: JoinRealtimeInfo,
}

/// Realtime subscription and recovery resources for a joined (or waiting) participant.
/// Paths are relative to the API origin; authorization is re-evaluated on every call.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JoinRealtimeInfo {
    /// Whether the conference WebSocket stream is available (`FEATURE_CONFERENCE_REALTIME_ENABLED`).
    pub enabled: bool,
    /// `POST` → one-shot WebSocket ticket (present when `enabled`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_path: Option<String>,
    /// WebSocket stream (`?ticket=…&cursor=…`; present when `enabled`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_path: Option<String>,
    /// REST replay of missed events (`?after=<sequence>`; present when `enabled`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events_path: Option<String>,
    /// Canonical conference state — the primary recovery resource.
    pub conference_path: String,
    /// The caller's own participant state (admission decision fallback when polling).
    pub participant_path: String,
}

impl JoinRealtimeInfo {
    pub fn for_conference(conference_id: Uuid, realtime_enabled: bool) -> Self {
        let base = format!("/api/v2/conferences/{conference_id}");
        Self {
            enabled: realtime_enabled,
            ticket_path: realtime_enabled.then(|| format!("{base}/realtime/ticket")),
            stream_path: realtime_enabled.then(|| format!("{base}/realtime")),
            events_path: realtime_enabled.then(|| format!("{base}/events")),
            participant_path: format!("{base}/participants/me"),
            conference_path: base,
        }
    }
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
    /// Once the Organizations Service is authoritative: the member's full role (OWNER,
    /// ADMIN, MANAGER, MEMBER or GUEST); `role` then shows MANAGER and GUEST as MEMBER.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_role: Option<String>,
    /// Once the Organizations Service is authoritative: the membership version, used as
    /// `expectedVersion` by `PATCH /api/v2/organizations/{id}/members/{userId}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// ORG-006: the member's display name (the account's chosen display name, otherwise
    /// its username), from the account authority. `null` when the account no longer
    /// exists.
    pub display_name: Option<String>,
    /// ORG-006: the member's username (public handle). `null` when the account no
    /// longer exists.
    pub username: Option<String>,
    /// The member's account email address. Present ONLY when the caller may see member
    /// emails: an ACTIVE OWNER or ADMIN of this organization, or a platform
    /// administrator. Absent (not `null`) for every other caller and for accounts that no
    /// longer exist. Enforced by the backend; never infer it from the client role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyOrganizationResponse {
    pub organization: OrganizationResponse,
    pub role: OrganizationRole,
    /// Once the Organizations Service is authoritative: the caller's full role (OWNER,
    /// ADMIN, MANAGER or MEMBER); `role` then shows MANAGER as MEMBER.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_role: Option<String>,
}

// ---------------------------------------------------------------------------
// First-party organizations (Blueprint Section 4; relayed to the Organizations Service)
//
// These schemas document the first-party organization routes, which meeting-service
// relays to the Organizations Service unchanged (Organizations validates every field and
// enforces every role rule). They are never deserialized here.
// ---------------------------------------------------------------------------

/// Optional legal registration of an organization (self-declared until an operator
/// verifies it; never the CF Meet creation time).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationLegalRegistration {
    /// Up to 200 characters.
    pub registered_name: Option<String>,
    /// Up to 100 characters.
    pub registration_number: Option<String>,
    /// `YYYY-MM-DD`.
    pub incorporated_on: Option<String>,
    /// `SELF_DECLARED` or `VERIFIED` (read-only; set by an operator).
    pub verification: Option<String>,
}

/// Organization profile. On update, an omitted field is unchanged and `null` clears it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationProfile {
    /// Up to 2000 characters.
    pub description: Option<String>,
    /// Up to 60 characters.
    pub organization_type: Option<String>,
    /// Up to 60 characters.
    pub industry: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: Option<String>,
    /// Up to 120 characters.
    pub city: Option<String>,
    /// Up to 500 characters.
    pub operating_address: Option<String>,
    /// Up to 500 characters.
    pub logo_ref: Option<String>,
    /// Up to 254 characters.
    pub public_contact_email: Option<String>,
    pub legal_registration: Option<OrganizationLegalRegistration>,
}

/// `POST /api/v2/organizations`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateBusinessOrganizationRequest {
    /// 1–200 characters.
    pub name: String,
    /// 3–63 lowercase letters, digits and `-`, starting and ending with a letter or digit.
    pub slug: String,
    pub profile: Option<OrganizationProfile>,
}

/// `PATCH /api/v2/organizations/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBusinessOrganizationRequest {
    /// The organization `version` the change is based on (optimistic concurrency).
    pub expected_version: i64,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub profile: Option<OrganizationProfile>,
}

/// The caller's own membership, as seen on an organization.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationViewer {
    pub membership_id: Uuid,
    /// OWNER, ADMIN, MANAGER, MEMBER or GUEST.
    pub role: String,
    /// ACTIVE or SUSPENDED.
    pub status: String,
    /// Capabilities the caller holds in this organization (default deny).
    pub capabilities: Vec<String>,
}

/// A first-party business organization (the Organizations Service resource).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BusinessOrganization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    /// ACTIVE, SUSPENDED or ARCHIVED.
    pub lifecycle_status: String,
    /// NOT_REQUIRED, PENDING, IN_PROGRESS, READY or FAILED.
    pub provisioning_state: String,
    /// SELF_SERVICE, PLATFORM_ADMIN or IMPORTED.
    pub creation_source: String,
    pub created_by: Option<Uuid>,
    /// When the organization was created on CF Meet (never an incorporation date).
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
    pub archived_at: Option<DateTime<Utc>>,
    pub profile: Option<OrganizationProfile>,
    /// Present when the caller is a member.
    pub viewer: Option<OrganizationViewer>,
    pub member_count: Option<i64>,
    /// `GET /api/v2/organizations/{id}` only: the display name of `createdBy` from the
    /// account authority (`null` when there is no creator or the account no longer
    /// exists). Never an email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_display_name: Option<String>,
}

/// A page of organizations (platform administrators).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BusinessOrganizationPage {
    pub items: Vec<BusinessOrganization>,
    /// Pass as `cursor` for the next page; null on the last page.
    pub next_cursor: Option<Uuid>,
}

/// One membership of a first-party organization.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BusinessOrganizationMembership {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub user_id: Uuid,
    /// OWNER, ADMIN, MANAGER, MEMBER or GUEST.
    pub role: String,
    /// ACTIVE, SUSPENDED or REMOVED.
    pub status: String,
    /// CREATOR, INVITATION, IMPORTED, OPERATOR or OWNERSHIP_TRANSFER.
    pub joined_via: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

/// `PATCH /api/v2/organizations/{id}/members/{userId}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBusinessMembershipRequest {
    /// The membership `version` the change is based on.
    pub expected_version: i64,
    /// OWNER, ADMIN, MANAGER, MEMBER or GUEST.
    pub role: Option<String>,
    /// ACTIVE, SUSPENDED or REMOVED.
    pub status: Option<String>,
}

/// `POST /api/v2/organizations/{id}/ownership-transfer`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationOwnershipTransferRequest {
    /// The membership that becomes OWNER.
    pub to_membership_id: Uuid,
    /// The organization `version` the transfer is based on.
    pub expected_version: i64,
}

/// `POST /api/v2/organizations/{id}/invitations`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrganizationInvitationRequest {
    /// The invitee's email address; only an account with this verified email can accept.
    /// Send either `email`, or `identifierType` + `identifier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// `EMAIL` or `CF_MEET_ID`, together with `identifier` (instead of `email`). A
    /// `CF_MEET_ID` invitation is resolved server-side through the Communications Hub
    /// under the consumer-discovery rules (discoverable id, visible profile, no block,
    /// active account); anything the inviter may not discover is `404
    /// INVITEE_NOT_FOUND`, indistinguishably. The invitee's email is never returned to
    /// the inviter, not even masked: the invitation shows `CF Meet ID <id>` as its hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier_type: Option<String>,
    /// The email address or the CF Meet ID (trimmed; emails are lower-cased).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// ADMIN, MANAGER, MEMBER or GUEST.
    pub role: String,
    /// Blueprint Section 11: also email the invitation link to `email` (default false).
    /// Handled by this relay; the outcome is reported in `emailDelivery`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_email: Option<bool>,
}

/// An organization invitation. The raw `token` is returned once, at creation only.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationInvitation {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub role: String,
    /// PENDING, ACCEPTED, DECLINED, REVOKED or EXPIRED.
    pub status: String,
    /// Masked invitee email, e.g. `a***@example.com`; `CF Meet ID <id>` for an invitation
    /// addressed by CF Meet ID (the email is never shown, not even masked).
    pub invitee_email_hint: String,
    pub invited_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    /// One-time invitation token (creation response only; never in URLs or logs).
    pub token: Option<String>,
    /// Creation with `sendEmail: true` only: DISABLED, ACCEPTED or FAILED.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_delivery: Option<EmailDelivery>,
    /// Creation by identifier only: the `identifierType` used (`EMAIL` or `CF_MEET_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier_type: Option<String>,
    /// Creation by `CF_MEET_ID` only: the CF Meet ID the invitation was addressed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
}

/// `POST /api/v2/organization-invitations/accept` and `/decline`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationInvitationTokenRequest {
    /// The invitation token received by the invitee.
    pub token: String,
}

/// Suspend, reactivate or archive an organization.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationLifecycleRequest {
    /// Optional organization `version` the change is based on.
    pub expected_version: Option<i64>,
}

/// `GET /api/v2/organizations/{id}/invitations`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationInvitationList {
    /// Invitations (masked invitee email; never a token).
    pub items: Vec<OrganizationInvitation>,
}

/// Who performed an audited organization change.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationAuditActor {
    /// USER, OPERATOR or SYSTEM.
    pub kind: String,
    pub user_id: Option<Uuid>,
    /// The service the change came through (for example `cfmeet.meeting-service`).
    pub service_id: String,
}

/// One organization audit event (append-only).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationAuditEvent {
    pub id: Uuid,
    /// Monotonic sequence number (pass the oldest one seen as `before`).
    pub seq: i64,
    pub organization_id: Option<Uuid>,
    /// Dotted action name, for example `membership.role_changed` or `policy.updated`.
    pub action: String,
    pub actor: OrganizationAuditActor,
    /// The kind of resource changed (organization, membership, invitation, team, policy...).
    pub target_type: String,
    pub target_id: Option<Uuid>,
    /// Value-free change summary (ids, versions, changed field names).
    #[schema(value_type = Object)]
    pub summary: serde_json::Value,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

/// A page of organization audit events, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationAuditEventPage {
    pub items: Vec<OrganizationAuditEvent>,
    /// Pass as `before` for the next (older) page; null on the last page.
    pub next_cursor: Option<i64>,
}

/// The organization's technical scope mapping (ids and verification state only).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationScopeMapping {
    pub id: Uuid,
    /// `primary`.
    pub capability: String,
    /// PENDING, VERIFYING, READY, FAILED, SUSPENDED or RETIRED.
    pub mapping_state: String,
    pub mapping_version: i64,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub verified_at: Option<DateTime<Utc>>,
    /// ACTIVE or SUSPENDED.
    pub tenant_status_desired: Option<String>,
    /// ACTIVE or SUSPENDED.
    pub tenant_status_applied: Option<String>,
    pub sync_attempts: i64,
    pub last_sync_error_class: Option<String>,
}

/// `GET /api/v2/organizations/{id}/provisioning`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationProvisioning {
    pub organization_id: Uuid,
    /// NOT_REQUIRED, PENDING, IN_PROGRESS, READY or FAILED.
    pub provisioning_state: String,
    pub workflow_id: Option<Uuid>,
    /// PENDING, IN_PROGRESS, READY, FAILED or CANCELLED.
    pub workflow_state: Option<String>,
    /// The current step (at most 64 characters).
    pub step: Option<String>,
    pub attempts: i64,
    /// The failure class of the last failed attempt.
    pub last_error_class: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// Whether technical provisioning runs in this deployment (while false organizations
    /// stay PENDING).
    pub provisioning_enabled: bool,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub scopes: Vec<OrganizationScopeMapping>,
}

// ---------------------------------------------------------------------------
// Teams and policies (Blueprint Section 6; relayed to the Organizations Service)
// ---------------------------------------------------------------------------

/// A team of a first-party organization.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationTeam {
    pub id: Uuid,
    pub organization_id: Uuid,
    /// 1–120 characters, unique among the organization's ACTIVE teams.
    pub name: String,
    /// Up to 500 characters.
    pub description: Option<String>,
    pub parent_team_id: Option<Uuid>,
    /// ACTIVE or ARCHIVED.
    pub status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Pass as `expectedVersion` to change or archive the team.
    pub version: i64,
    /// ACTIVE team members.
    pub member_count: i64,
}

/// A page of teams (id order).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationTeamPage {
    pub items: Vec<OrganizationTeam>,
    /// Pass as `cursor` for the next page; null on the last page.
    pub next_cursor: Option<Uuid>,
}

/// `POST /api/v2/organizations/{id}/teams`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrganizationTeamRequest {
    /// 1–120 characters.
    pub name: String,
    /// Up to 500 characters.
    pub description: Option<String>,
    /// An ACTIVE team of the same organization (nesting is bounded).
    pub parent_team_id: Option<Uuid>,
}

/// `PATCH /api/v2/organizations/{id}/teams/{teamId}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOrganizationTeamRequest {
    /// The team `version` the change is based on.
    pub expected_version: i64,
    pub name: Option<String>,
    /// `null` clears the description.
    pub description: Option<String>,
    /// `null` detaches the team from its parent; absent leaves it unchanged.
    pub parent_team_id: Option<Uuid>,
}

/// `POST /api/v2/organizations/{id}/teams/{teamId}/archive`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveOrganizationTeamRequest {
    /// Optional team `version` the archive is based on.
    pub expected_version: Option<i64>,
}

/// One team membership. `displayName` / `username` are added by this relay from the
/// account authority (never an email address).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationTeamMember {
    /// The team membership id (the `{memberId}` path segment).
    pub id: Uuid,
    pub organization_id: Uuid,
    pub team_id: Uuid,
    /// The organization membership.
    pub membership_id: Uuid,
    pub user_id: Uuid,
    /// TEAM_MANAGER or TEAM_MEMBER.
    pub role: String,
    /// ACTIVE or REMOVED.
    pub status: String,
    pub added_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    /// Pass as `expectedVersion` to change the team role.
    pub version: i64,
    /// The member's display name; `null` when the account no longer exists.
    pub display_name: Option<String>,
    /// The member's username; `null` when the account no longer exists.
    pub username: Option<String>,
}

/// A page of ACTIVE team members (id order).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationTeamMemberPage {
    pub items: Vec<OrganizationTeamMember>,
    /// Pass as `cursor` for the next page; null on the last page.
    pub next_cursor: Option<Uuid>,
}

/// `POST /api/v2/organizations/{id}/teams/{teamId}/members` — exactly one of
/// `membershipId` and `userId`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddOrganizationTeamMemberRequest {
    /// An ACTIVE organization membership.
    pub membership_id: Option<Uuid>,
    /// A user with an ACTIVE organization membership.
    pub user_id: Option<Uuid>,
    /// TEAM_MANAGER (organization MANAGERs, ADMINs and OWNERs only) or TEAM_MEMBER.
    pub role: String,
}

/// `PATCH /api/v2/organizations/{id}/teams/{teamId}/members/{memberId}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOrganizationTeamMemberRequest {
    /// The team membership `version` the change is based on.
    pub expected_version: i64,
    /// TEAM_MANAGER or TEAM_MEMBER.
    pub role: String,
}

/// Who may list the organization's members.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyMemberDirectory {
    /// MEMBERS (default), MANAGERS or ADMINS: the minimum role that may list members.
    pub visibility: String,
    /// Colleague discovery hint (default true).
    pub searchable: bool,
}

/// External guests.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyGuests {
    /// GUEST invitations may be created or accepted (default true).
    pub invitations_allowed: bool,
}

/// Organization meetings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyMeetings {
    /// MEMBERS (default), MANAGERS or ADMINS: the minimum role that may host.
    pub host_creation: String,
}

/// Features enabled for the organization (a disabled feature removes its capabilities
/// for every role).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyFeatures {
    pub meetings: bool,
    pub messaging: bool,
    pub calling: bool,
}

/// Retention hint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyRetention {
    /// 1–3650 days, or `null` for no hint.
    pub hint_days: Option<i64>,
}

/// Delegated administration (OWNER-only section).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyDelegatedAdministration {
    /// MANAGERs may invite MEMBERs and GUESTs (default true).
    pub managers_may_invite: bool,
    /// MANAGERs may create teams (default false).
    pub managers_may_create_teams: bool,
    /// ADMINs may change policies (default true).
    pub admins_may_manage_policies: bool,
}

/// The organization policy document (schema version 1). Defaults equal the role matrix; a
/// policy only removes capabilities and never touches OWNER administration.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicyDocument {
    pub member_directory: OrganizationPolicyMemberDirectory,
    pub guests: OrganizationPolicyGuests,
    pub meetings: OrganizationPolicyMeetings,
    pub features: OrganizationPolicyFeatures,
    pub retention: OrganizationPolicyRetention,
    pub delegated_administration: OrganizationPolicyDelegatedAdministration,
}

/// `GET /api/v2/organizations/{id}/policies`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicy {
    /// Always 1.
    pub schema_version: i64,
    pub organization_id: Uuid,
    /// Pass as `expectedRevision` to change the policy (0: never changed, defaults).
    pub revision: i64,
    pub policy: OrganizationPolicyDocument,
    pub updated_by: Option<Uuid>,
    pub updated_at: Option<DateTime<Utc>>,
    /// The Meeting Service profile policy, verbatim (still enforced by the Meeting
    /// Service).
    #[schema(value_type = Object)]
    pub legacy_meeting_profile_policy: serde_json::Value,
}

/// `PATCH /api/v2/organizations/{id}/policies` (compare-and-swap).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOrganizationPolicyRequest {
    /// The policy `revision` the change is based on; a stale one is VERSION_CONFLICT.
    pub expected_revision: i64,
    /// A partial policy document merged into the current one (unknown keys refused).
    #[schema(value_type = Object)]
    pub policy: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Workspace context (Blueprint Section 8, ADR 0056 §5)
//
// A workspace is a per-request context: "Personal" or one organization the caller is an
// ACTIVE member of. The server never stores an "active organization"; every
// organization-scoped request still names its organization and is re-verified.
// ---------------------------------------------------------------------------

/// Kind of workspace context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkspaceKind {
    /// The caller's own personal context (`id` = `personal`).
    Personal,
    /// A first-party business organization (`id` = the organization id).
    Organization,
}

/// Which store answered: the meeting-service organization directory
/// (`ORGANIZATIONS_AUTHORITY_MODE` legacy / shadow) or the Organizations Service
/// (authoritative).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkspaceAuthority {
    LegacyDirectory,
    OrganizationsService,
}

/// Why a capability is (not) granted in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkspaceCapabilityReason {
    Allowed,
    /// The caller's role does not hold it.
    CapabilityNotGranted,
    /// The role holds it but the organization policy removes it.
    PolicyRestricted,
    /// The organization is suspended: it grants nothing (except what the authority still
    /// grants its OWNERs, such as `organization.read`).
    OrganizationSuspended,
    /// Not available in this deployment (the Organizations Service is not authoritative).
    FeatureDisabled,
}

/// A small summary of what the caller can do in a workspace.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCapabilitySummary {
    /// Every capability granted now (default deny).
    pub granted: Vec<String>,
    /// Meetings can be created in this context.
    pub can_create_meetings: bool,
    /// Members can be invited (`organization.members.invite`).
    pub can_invite_members: bool,
    /// Organization administration: profile, member management or policies.
    pub can_administer: bool,
}

/// One workspace context of the caller.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// `personal`, or the organization id.
    pub id: String,
    pub kind: WorkspaceKind,
    /// `Personal`, or the organization name.
    pub display_name: String,
    /// ORGANIZATION only.
    pub organization_id: Option<Uuid>,
    /// ORGANIZATION only.
    pub slug: Option<String>,
    /// The caller's role (OWNER, ADMIN, MANAGER, MEMBER or GUEST; the legacy directory knows
    /// OWNER, ADMIN and MEMBER). ORGANIZATION only.
    pub role: Option<String>,
    /// ACTIVE or SUSPENDED (a suspended organization grants nothing). ORGANIZATION only.
    pub organization_status: Option<String>,
    /// NOT_REQUIRED, PENDING, IN_PROGRESS, READY or FAILED, from the Organizations Service;
    /// null for PERSONAL and when the legacy directory answers (no provisioning there).
    pub provisioning_state: Option<String>,
    pub capabilities: WorkspaceCapabilitySummary,
}

/// `GET /api/v2/me/workspaces`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePage {
    /// PERSONAL first, then organizations ordered by id.
    pub items: Vec<Workspace>,
    /// Pass back as `cursor` for the next page; null on the last page.
    pub next_cursor: Option<String>,
    pub authority: WorkspaceAuthority,
}

/// `GET /api/v2/me/workspaces` paging.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceListQuery {
    /// Opaque cursor from a previous page's `nextCursor`.
    pub cursor: Option<String>,
    /// Page size, 1-200 (default 50).
    pub limit: Option<u32>,
}

/// One capability decision.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCapability {
    /// Capability name, e.g. `meetings.organization.create`.
    pub capability: String,
    pub granted: bool,
    pub reason: WorkspaceCapabilityReason,
}

/// Features the workspace context does not disable. Owner services still apply their
/// own policy (stricter wins) and re-verify every request.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFeatures {
    pub meetings: bool,
    pub messaging: bool,
    pub calling: bool,
}

/// `GET /api/v2/me/workspaces/{workspaceId}/capabilities`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCapabilities {
    /// `personal`, or the organization id.
    pub workspace_id: String,
    pub kind: WorkspaceKind,
    pub authority: WorkspaceAuthority,
    pub organization_id: Option<Uuid>,
    pub role: Option<String>,
    pub organization_status: Option<String>,
    pub provisioning_state: Option<String>,
    /// Organization policy revision the answer was computed with (Organizations Service).
    pub policy_revision: Option<i64>,
    pub features: WorkspaceFeatures,
    /// Every capability of the vocabulary with its decision.
    pub capabilities: Vec<WorkspaceCapability>,
    /// PERSONAL only: the Communications Hub read that owns personal communications
    /// capabilities (`/api/v2/communications/me/capabilities`).
    pub personal_communications_capabilities: Option<String>,
    /// When the answer was computed (it is never cached server-side).
    pub evaluated_at: DateTime<Utc>,
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
    pub track_kind: TrackKind,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MuteResponse {
    pub participant_id: Uuid,
    pub track_kind: TrackKind,
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
    pub events: Vec<RealtimeEnvelope>,
    /// Cursor to resume from.
    pub cursor: i64,
    /// True when the cursor cannot be served: refetch `canonical` and resume from `cursor`.
    pub resync_required: bool,
    pub canonical: String,
}

pub use super::v2_collab::*;
pub use super::v2_ghi::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{json, Value};

    /// Every request DTO accepts its minimal valid body and rejects any unknown or
    /// authority-bearing field (identity, role, ownership, provider, storage).
    fn strict<T: DeserializeOwned>(name: &str, valid: Value) {
        serde_json::from_value::<T>(valid.clone())
            .unwrap_or_else(|e| panic!("{name}: valid body rejected: {e}"));
        for injected in [
            "userId",
            "ownerUserId",
            "participantId",
            "admissionState",
            "rtcRoomName",
            "storageKey",
            "providerRef",
            "unexpected",
        ] {
            let mut body = valid.clone();
            if body.get(injected).is_some() {
                continue; // a legitimate field of this DTO (e.g. AddMember.userId)
            }
            body[injected] = json!("x");
            assert!(
                serde_json::from_value::<T>(body).is_err(),
                "{name} accepts injected field `{injected}`"
            );
        }
    }

    #[test]
    fn request_dtos_reject_unknown_and_authority_fields() {
        let id = Uuid::new_v4();
        strict::<CreateMeetingSpaceRequest>("CreateMeetingSpace", json!({ "title": "t" }));
        strict::<UpdateMeetingSpaceRequest>("UpdateMeetingSpace", json!({ "version": 1 }));
        strict::<OwnerTransferRequest>(
            "OwnerTransfer",
            json!({ "newOwnerUserId": id, "version": 1 }),
        );
        strict::<InstantMeetingRequest>("InstantMeeting", json!({}));
        strict::<AddMemberRequest>("AddMember", json!({ "userId": id, "role": "PARTICIPANT" }));
        strict::<UpdateMemberRequest>("UpdateMember", json!({ "role": "HOST", "version": 1 }));
        strict::<CreateInvitationRequest>("CreateInvitation", json!({}));
        strict::<CreateConferenceRequest>("CreateConference", json!({}));
        strict::<RescheduleConferenceRequest>(
            "Reschedule",
            json!({ "scheduledStartAt": "2030-01-01T10:00:00Z" }),
        );
        strict::<SetRecurrenceRequest>(
            "SetRecurrence",
            json!({ "rrule": "FREQ=DAILY", "startLocal": "2030-01-01T10:00:00", "durationMinutes": 30 }),
        );
        strict::<JoinConferenceRequest>("Join", json!({}));
        strict::<LeaveConferenceRequest>("Leave", json!({}));
        strict::<AdmitRequest>("Admit", json!({ "participantIds": [id] }));
        strict::<DenyRequest>("Deny", json!({ "participantIds": [id] }));
        strict::<RoleChangeRequest>("RoleChange", json!({ "role": "PRESENTER" }));
        strict::<RemoveParticipantRequest>("Remove", json!({}));
        strict::<BanParticipantRequest>("Ban", json!({ "scope": "CONFERENCE" }));
    }

    #[test]
    fn request_dtos_reject_invalid_enum_values_and_types() {
        assert!(serde_json::from_value::<RoleChangeRequest>(json!({ "role": "Host" })).is_err());
        assert!(
            serde_json::from_value::<RoleChangeRequest>(json!({ "role": "SUPERUSER" })).is_err()
        );
        assert!(serde_json::from_value::<UpdateMemberRequest>(
            json!({ "role": "HOST", "version": "1" })
        )
        .is_err());
        assert!(
            serde_json::from_value::<AdmitRequest>(json!({ "participantIds": ["nope"] })).is_err()
        );
        assert!(serde_json::from_value::<CreateMeetingSpaceRequest>(
            json!({ "title": "t", "accessPolicy": "public" })
        )
        .is_err());
    }

    #[test]
    fn join_realtime_info_is_scoped_to_the_conference() {
        let cid = Uuid::new_v4();
        let on = JoinRealtimeInfo::for_conference(cid, true);
        assert_eq!(on.conference_path, format!("/api/v2/conferences/{cid}"));
        assert!(on.ticket_path.unwrap().ends_with("/realtime/ticket"));
        let off = JoinRealtimeInfo::for_conference(cid, false);
        assert!(
            off.ticket_path.is_none() && off.stream_path.is_none() && off.events_path.is_none()
        );
        assert!(off.participant_path.ends_with("/participants/me"));
    }
}
