use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[derive(OpenApi)]
#[openapi(
    paths(
        // General endpoints
        crate::handlers::root::get_app_info,
        crate::handlers::root::health_check,
        // Authentication endpoints
        crate::handlers::auth::register,
        crate::handlers::auth::verify_email,
        crate::handlers::auth::login,
        crate::handlers::auth::refresh_token,
        crate::handlers::auth::logout,
        crate::handlers::auth::resend_verification_email,
        crate::handlers::auth::forgot_password,
        crate::handlers::auth::verify_reset_token,
        crate::handlers::auth::reset_password,
        // OAuth endpoints
        crate::handlers::auth::oauth_google_init,
        crate::handlers::auth::oauth_google_callback,
        // API Key endpoints
        crate::handlers::api_keys::create_api_key,
        crate::handlers::api_keys::list_api_keys,
        crate::handlers::api_keys::get_api_key,
        crate::handlers::api_keys::revoke_api_key,
        // API (third-party) endpoints
        crate::handlers::meetings::api_create_meeting,
        crate::handlers::meetings::api_join_meeting,
        // Meeting endpoints
        crate::handlers::meetings::create_instant_meeting,
        crate::handlers::meetings::list_meetings,
        crate::handlers::meetings::create_meeting,
        crate::handlers::meetings::get_meeting,
        crate::handlers::meetings::update_meeting,
        crate::handlers::meetings::delete_meeting,
        crate::handlers::meetings::join_meeting,
        crate::handlers::meetings::join_meeting_by_identifier,
        crate::handlers::meetings::end_meeting,
        crate::handlers::meetings::get_public_meeting_info,
        // Participant endpoints
        crate::handlers::participants::list_participants,
        crate::handlers::participants::kick_participant,
        crate::handlers::participants::list_waiting_participants,
        crate::handlers::participants::admit_participant,
        crate::handlers::participants::deny_participant,
        crate::handlers::participants::update_audio,
        crate::handlers::participants::update_video,
        crate::handlers::participants::start_screen_share,
        crate::handlers::participants::stop_screen_share,
        // Chat endpoints
        crate::handlers::chat::send_message,
        crate::handlers::chat::get_messages,
        // User endpoints
        crate::handlers::users::get_current_user,
        crate::handlers::users::update_current_user,
        crate::handlers::users::deactivate_account,
        // Webhook endpoints
        crate::handlers::webhooks::create_webhook,
        crate::handlers::webhooks::list_webhooks,
        crate::handlers::webhooks::update_webhook,
        crate::handlers::webhooks::delete_webhook,
        // Admin endpoints
        crate::handlers::admin::list_users,
        crate::handlers::admin::update_user,
        crate::handlers::admin::list_meetings,
        crate::handlers::admin::list_logs,
        // Phase 2 meeting domain (/api/v2)
        crate::handlers::v2::spaces::create_meeting_space,
        crate::handlers::v2::spaces::list_meeting_spaces,
        crate::handlers::v2::spaces::get_meeting_space,
        crate::handlers::v2::spaces::resolve_identifier,
        crate::handlers::v2::spaces::update_meeting_space,
        crate::handlers::v2::spaces::archive_meeting_space,
        crate::handlers::v2::spaces::duplicate_meeting_space,
        crate::handlers::v2::spaces::transfer_owner,
        crate::handlers::v2::spaces::create_instant_meeting,
        crate::handlers::v2::spaces::set_recurrence,
        crate::handlers::v2::spaces::get_recurrence,
        crate::handlers::v2::spaces::end_recurrence,
        crate::handlers::v2::members::list_members,
        crate::handlers::v2::members::add_member,
        crate::handlers::v2::members::update_member,
        crate::handlers::v2::members::revoke_member,
        crate::handlers::v2::invitations::create_invitation,
        crate::handlers::v2::invitations::list_invitations,
        crate::handlers::v2::invitations::revoke_invitation,
        crate::handlers::v2::invitations::resolve_invitation,
        crate::handlers::v2::conferences::create_conference,
        crate::handlers::v2::conferences::list_conferences,
        crate::handlers::v2::conferences::get_conference,
        crate::handlers::v2::conferences::prepare_conference,
        crate::handlers::v2::conferences::start_conference,
        crate::handlers::v2::conferences::lock_conference,
        crate::handlers::v2::conferences::unlock_conference,
        crate::handlers::v2::conferences::end_conference,
        crate::handlers::v2::conferences::cancel_conference,
        crate::handlers::v2::conferences::reschedule_conference,
        crate::handlers::v2::conferences::join_conference,
        crate::handlers::v2::conferences::leave_conference,
        crate::handlers::v2::participants::get_me,
        crate::handlers::v2::participants::list_participants,
        crate::handlers::v2::participants::get_waiting_room,
        crate::handlers::v2::participants::admit,
        crate::handlers::v2::participants::deny,
        crate::handlers::v2::participants::return_to_waiting,
        crate::handlers::v2::participants::change_role,
        crate::handlers::v2::participants::remove,
        crate::handlers::v2::participants::ban,
        crate::handlers::v2::compat::shadow_compare,
        crate::handlers::v2::compat::run_backfill,
        crate::handlers::v2::spaces::set_recurrence_exception,
        crate::handlers::v2::spaces::list_recurrence_exceptions,
        crate::handlers::v2::organizations::create_organization,
        crate::handlers::v2::organizations::update_organization,
        crate::handlers::v2::organizations::list_organization_members,
        crate::handlers::v2::organizations::upsert_organization_member,
        crate::handlers::v2::organizations::my_organizations,
    ),
    components(
        schemas(
            // Auth schemas
            crate::dto::RegisterRequest,
            crate::dto::RegisterResponse,
            crate::dto::LoginRequest,
            crate::dto::LoginResponse,
            crate::dto::UserInfo,
            crate::dto::VerifyEmailRequest,
            crate::dto::RefreshTokenRequest,
            crate::dto::RefreshTokenResponse,
            crate::dto::MessageResponse,
            crate::dto::ResendVerificationRequest,
            crate::dto::ForgotPasswordRequest,
            crate::dto::VerifyResetTokenRequest,
            crate::dto::VerifyResetTokenResponse,
            crate::dto::ResetPasswordRequest,
            // API Key schemas
            crate::dto::CreateApiKeyRequest,
            crate::dto::CreateApiKeyResponse,
            crate::dto::CreateApiKeyApiResponse,
            crate::dto::ApiKeyResponse,
            crate::dto::ApiKeyApiResponse,
            crate::dto::ListApiKeysResponse,
            crate::dto::RevokeApiKeyResponse,
            // API (third-party) schemas
            crate::handlers::meetings::ApiCreateMeetingRequest,
            crate::handlers::meetings::ApiJoinMeetingRequest,
            // Meeting schemas
            crate::dto::InstantMeetingRequest,
            crate::dto::InstantMeetingResponse,
            crate::dto::InstantMeetingApiResponse,
            crate::dto::CreateMeetingRequest,
            crate::dto::UpdateMeetingRequest,
            crate::dto::JoinMeetingRequest,
            crate::dto::MeetingResponse,
            crate::dto::JoinMeetingResponse,
            crate::dto::EndMeetingResponse,
            crate::dto::MeetingApiResponse,
            crate::dto::JoinMeetingApiResponse,
            crate::dto::EndMeetingApiResponse,
            crate::dto::PaginatedMeetingResponse,
            crate::dto::MeetingPaginationMeta,
            crate::dto::PublicMeetingInfoResponse,
            crate::dto::PublicMeetingInfoApiResponse,
            // Participant schemas
            crate::dto::DetailedParticipantResponse,
            crate::dto::ParticipantsListResponse,
            crate::dto::ParticipantActionResponse,
            crate::dto::WaitingParticipantResponse,
            crate::dto::WaitingListResponse,
            crate::dto::UpdateAudioRequest,
            crate::dto::UpdateVideoRequest,
            crate::dto::MediaStateResponse,
            crate::dto::ScreenShareResponse,
            // Chat schemas
            crate::dto::SendChatMessageRequest,
            crate::dto::ChatMessageResponse,
            crate::dto::SendChatMessageApiResponse,
            crate::dto::ChatMessagesListResponse,
            // User schemas
            crate::dto::UserMeResponse,
            crate::dto::UpdateMeRequest,
            // Webhook schemas
            crate::dto::CreateWebhookRequest,
            crate::dto::UpdateWebhookRequest,
            crate::dto::WebhookResponse,
            crate::dto::CreateWebhookResponse,
            crate::dto::WebhookListResponse,
            crate::dto::WebhookApiResponse,
            crate::dto::CreateWebhookApiResponse,
            // Admin schemas
            crate::dto::AdminUserResponse,
            crate::dto::AdminUpdateUserRequest,
            crate::dto::AdminPaginationMeta,
            crate::dto::AdminUserListResponse,
            crate::dto::AdminUserApiResponse,
            crate::dto::AdminMeetingResponse,
            crate::dto::AdminMeetingListResponse,
            crate::dto::AdminLogResponse,
            crate::dto::AdminLogListResponse,
            // Phase 2 meeting domain (/api/v2)
            crate::dto::v2::AccessConfig,
            crate::dto::v2::SchedulePolicy,
            crate::dto::v2::CreateMeetingSpaceRequest,
            crate::dto::v2::UpdateMeetingSpaceRequest,
            crate::dto::v2::VersionedRequest,
            crate::dto::v2::DuplicateMeetingSpaceRequest,
            crate::dto::v2::OwnerTransferRequest,
            crate::dto::v2::InstantMeetingRequest,
            crate::dto::v2::ResourceView,
            crate::dto::v2::MeetingSpaceResponse,
            crate::dto::v2::MeetingSpaceListResponse,
            crate::dto::v2::InstantMeetingResponse,
            crate::dto::v2::AddMemberRequest,
            crate::dto::v2::UpdateMemberRequest,
            crate::dto::v2::MemberResponse,
            crate::dto::v2::MemberListResponse,
            crate::dto::v2::CreateInvitationRequest,
            crate::dto::v2::InvitationResponse,
            crate::dto::v2::CreateInvitationResponse,
            crate::dto::v2::ResolveInvitationResponse,
            crate::dto::v2::CreateConferenceRequest,
            crate::dto::v2::RescheduleConferenceRequest,
            crate::dto::v2::ListConferencesQuery,
            crate::dto::v2::ConferenceResponse,
            crate::dto::v2::ConferenceListResponse,
            crate::dto::v2::ProviderEnforcement,
            crate::dto::v2::ConferenceActionResponse,
            crate::dto::v2::SetRecurrenceRequest,
            crate::dto::v2::RecurrenceResponse,
            crate::dto::v2::JoinConferenceRequest,
            crate::dto::v2::JoinConferenceSummary,
            crate::dto::v2::ParticipantResponse,
            crate::dto::v2::ParticipantSessionResponse,
            crate::dto::v2::RtcCredentialResponse,
            crate::dto::v2::RtcStatus,
            crate::dto::v2::JoinPermissions,
            crate::dto::v2::JoinPolicy,
            crate::dto::v2::JoinConferenceResponse,
            crate::dto::v2::LeaveConferenceRequest,
            crate::dto::v2::LeaveConferenceResponse,
            crate::dto::v2::WaitingRoomResponse,
            crate::dto::v2::AdmitRequest,
            crate::dto::v2::DenyRequest,
            crate::dto::v2::SkippedParticipant,
            crate::dto::v2::BulkAdmissionResponse,
            crate::dto::v2::RoleChangeRequest,
            crate::dto::v2::RemoveParticipantRequest,
            crate::dto::v2::BanParticipantRequest,
            crate::dto::v2::ModerationResponse,
            crate::dto::v2::ResolveResponse,
            crate::dto::v2::ShadowComparisonResponse,
            crate::dto::v2::BackfillReportResponse,
            crate::dto::v2::RecurrenceExceptionAction,
            crate::dto::v2::RecurrenceExceptionRequest,
            crate::dto::v2::RecurrenceExceptionResponse,
            crate::domain::RecurrenceExceptionKind,
            crate::dto::v2::CreateOrganizationRequest,
            crate::dto::v2::UpdateOrganizationRequest,
            crate::dto::v2::UpsertOrganizationMemberRequest,
            crate::dto::v2::OrganizationResponse,
            crate::dto::v2::OrganizationMemberResponse,
            crate::dto::v2::MyOrganizationResponse,
            crate::domain::OrganizationStatus,
            crate::domain::OrganizationRole,
            crate::domain::OrganizationMemberStatus,
            crate::domain::MeetingType,
            crate::domain::SpaceStatus,
            crate::domain::AccessPolicy,
            crate::domain::WaitingRoomPolicy,
            crate::domain::SecurityProfile,
            crate::domain::MediaCapturePolicy,
            crate::domain::RecordOrigin,
            crate::domain::ConferenceStatus,
            crate::domain::ConferenceKind,
            crate::domain::TerminationReason,
            crate::domain::Role,
            crate::domain::RoleSource,
            crate::domain::IdentityType,
            crate::domain::AdmissionState,
            crate::domain::SessionState,
            crate::domain::PrincipalType,
            crate::domain::MemberStatus,
            crate::domain::InvitationStatus,
            crate::domain::BanScope,
            crate::domain::BanSubjectType,
            crate::domain::RecurrenceStatus,
            crate::domain::OutboxStatus,
            crate::error::ErrorEnvelope,
            crate::error::ErrorBody,
            crate::error::ErrorCode,
            crate::services::meeting_permissions::RolePolicy,
            crate::services::meeting_permissions::MediaPermissions,
        )
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "General", description = "Application information and health endpoints"),
        (name = "Authentication", description = "User authentication endpoints"),
        (name = "API Keys", description = "API key management for third-party integrations.\n\n## Usage\n- Create API keys via authenticated endpoints\n- Use `Api-Key: <key>` header for authentication\n- API keys have usage limits and expiration dates\n- Keys can be revoked at any time\n\n## Notes\n- Full API key is only shown on creation\n- Third-party devs cannot access internal media servers directly\n- Short-lived join tokens required for connecting to RTC infrastructure"),
        (name = "Meetings", description = "Meeting lifecycle management with LiveKit integration"),
        (name = "Participants", description = "Participant management (kick, list)"),
        (name = "Waiting Room", description = "Admit/Deny participants from waiting room"),
        (name = "Media Control", description = "Control audio, video, and screen sharing"),
        (name = "Chat", description = "In-meeting chat with hybrid real-time architecture.\n\n## Architecture\n- **LiveKit Data Channels**: Real-time message delivery via `publishData()` and `RoomEvent.DataReceived`\n- **REST API**: Message persistence and history loading for late joiners\n\n## Flow\n1. **Send**: POST /chat (persist) → LiveKit publishData() (broadcast)\n2. **Receive**: Listen to RoomEvent.DataReceived for instant delivery\n3. **Late Join**: GET /chat to load message history\n\n## Notes\n- Messages are **volatile** - automatically deleted when meeting ends\n- Only participants with 'joined' status can send/receive messages"),
        (name = "Users", description = "User profile management and account operations.\n\n## Endpoints\n- **GET /users/me**: Get current user profile\n- **PATCH /users/me**: Update profile (username only)\n- **POST /users/me/deactivate**: Deactivate account (soft delete)\n\n## Notes\n- All endpoints require JWT authentication\n- Deactivated users are blocked by auth middleware\n- Deactivation immediately invalidates all tokens"),
        (name = "Webhooks", description = "Webhook management for event notifications.\n\n## Events\n- `meeting_start`: Triggered when a meeting begins\n- `meeting_end`: Triggered when a meeting ends\n- `participant_join`: Triggered when a participant joins\n- `participant_leave`: Triggered when a participant leaves\n\n## Security\n- Each webhook has a unique secret for payload signing\n- Secret is only shown once on creation\n- Payloads are signed using HMAC-SHA256\n\n## Notes\n- Webhooks are user-scoped\n- Delivery is async and failure-tolerant"),
        (name = "Meeting Spaces (v2)", description = "Phase 2 persistent meeting definitions. A MeetingSpace is not a live call. Owner is always the authenticated caller; meeting links/codes identify a meeting but never grant membership or roles."),
        (name = "Scheduling (v2)", description = "RFC 5545 recurrence interpreted in the meeting space timezone; bounded, idempotent occurrence materialization."),
        (name = "Conferences (v2)", description = "ConferenceSession lifecycle: CREATED → PREPARING → WAITING → LIVE → ENDING → ENDED (+ CANCELLED / FAILED). ENDED is reached only after provider termination (Part E)."),
        (name = "Members (v2)", description = "Persistent meeting membership (not conference participation)."),
        (name = "Invitations (v2)", description = "Hash-only bearer invitations with expiry, revocation and transactional use limits."),
        (name = "Participants (v2)", description = "Logical conference participants (one per person per conference) and moderation. All participant ids are conference-scoped."),
        (name = "Waiting Room (v2)", description = "Waiting is a participant admission state. Access policy is always evaluated first."),
        (name = "Organizations (v2)", description = "Authoritative CF organization membership used by ORGANIZATION_ONLY access. Membership is resolved server-side; clients can never assert it."),
        (name = "Compatibility (v2)", description = "Legacy → v2 migration evidence and shadow comparison (platform administrators)."),
        (name = "Admin", description = "Administrative endpoints for system management.\n\n## Access\n- All endpoints require admin role\n- Role check is performed in each handler\n\n## Endpoints\n- **GET /admin/users**: List all users with pagination and filters\n- **PATCH /admin/users/:id**: Update user role or status\n- **GET /admin/meetings**: List all meetings with pagination and filters\n- **GET /admin/logs**: List session logs with pagination and filters\n\n## Notes\n- Admins cannot change their own role\n- Most operations are read-only")
    ),
    info(
        title = "CosmicForge RTC API",
        version = "1.1.0",
        description = "CF Meet Meeting Service. `/api/v1`: legacy meeting API (retained for compatibility). `/api/v2`: Phase 2 meeting domain (MeetingSpace → ConferenceSession → ConferenceParticipant → ParticipantSession). v2 errors use `{ success:false, error:{ code, message, requestId } }` with stable semantic codes.",
        contact(
            name = "CosmicForge Team",
            email = "support@cosmicforge.com"
        )
    )
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "participant_token",
                utoipa::openapi::security::SecurityScheme::ApiKey(
                    utoipa::openapi::security::ApiKey::Header(
                        utoipa::openapi::security::ApiKeyValue::new("X-Participant-Token"),
                    ),
                ),
            );
            components.add_security_scheme(
                "bearer_auth",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::Http::new(
                        utoipa::openapi::security::HttpAuthScheme::Bearer,
                    ),
                ),
            );
            components.add_security_scheme(
                "api_key_auth",
                utoipa::openapi::security::SecurityScheme::ApiKey(
                    utoipa::openapi::security::ApiKey::Header(
                        utoipa::openapi::security::ApiKeyValue::new("Api-Key"),
                    ),
                ),
            );
        }
    }
}

/// Deterministic OpenAPI JSON (no runtime server list) used for contract export.
pub fn openapi_json() -> String {
    ApiDoc::openapi()
        .to_pretty_json()
        .expect("OpenAPI document serializes")
}

/// Creates the Swagger UI router with dynamic server URL from environment
pub fn swagger_router(server_addr: &str) -> Router {
    let mut openapi = ApiDoc::openapi();

    // Set servers dynamically from environment
    let mut current_server = utoipa::openapi::Server::new(format!("http://{}", server_addr));
    current_server.description = Some("Current server".to_string());

    let mut prod_server = utoipa::openapi::Server::new("https://api.cosmicforge.com");
    prod_server.description = Some("Production server".to_string());

    openapi.servers = Some(vec![current_server, prod_server]);

    SwaggerUi::new("/swagger-ui")
        .url("/api-docs/openapi.json", openapi)
        .into()
}
