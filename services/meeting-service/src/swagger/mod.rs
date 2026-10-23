use axum::{
    http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE},
    response::{Html, IntoResponse, Redirect},
    routing::get,
    Router,
};
use utoipa::OpenApi;

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
        crate::handlers::auth::change_password,
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
        crate::handlers::v2::participants::mute,
        crate::handlers::v2::realtime::create_ticket,
        crate::handlers::v2::realtime::subscribe,
        crate::handlers::v2::realtime::replay_events,
        crate::handlers::v2::collaboration::audio_lock,
        crate::handlers::v2::collaboration::audio_unlock,
        crate::handlers::v2::collaboration::video_lock,
        crate::handlers::v2::collaboration::video_unlock,
        crate::handlers::v2::collaboration::screen_share_lock,
        crate::handlers::v2::collaboration::screen_share_unlock,
        crate::handlers::v2::collaboration::stop_screen_share,
        crate::handlers::v2::collaboration::spotlight,
        crate::handlers::v2::collaboration::remove_spotlight,
        crate::handlers::v2::collaboration::media_credential,
        crate::handlers::v2::collaboration::collaboration_state,
        crate::handlers::v2::collaboration::update_policy,
        crate::handlers::v2::collaboration::update_chat_policy,
        crate::handlers::v2::collaboration::pre_join,
        crate::handlers::v2::collaboration::list_messages,
        crate::handlers::v2::collaboration::send_message,
        crate::handlers::v2::collaboration::delete_message,
        crate::handlers::v2::collaboration::restrict_chat,
        crate::handlers::v2::collaboration::unrestrict_chat,
        crate::handlers::v2::collaboration::send_reaction,
        crate::handlers::v2::collaboration::raise_hand,
        crate::handlers::v2::collaboration::lower_hand,
        crate::handlers::v2::collaboration::lower_other_hand,
        crate::handlers::v2::collaboration::raised_hands,
        crate::handlers::v2::breakouts::list,
        crate::handlers::v2::breakouts::create,
        crate::handlers::v2::breakouts::assign,
        crate::handlers::v2::breakouts::move_participant,
        crate::handlers::v2::breakouts::patch_assignments,
        crate::handlers::v2::breakouts::open,
        crate::handlers::v2::breakouts::enter,
        crate::handlers::v2::breakouts::leave,
        crate::handlers::v2::breakouts::announce_all,
        crate::handlers::v2::breakouts::announce,
        crate::handlers::v2::breakouts::close,
        crate::handlers::v2::breakouts::close_all,
        crate::handlers::v2::engagement::create_poll,
        crate::handlers::v2::engagement::list_polls,
        crate::handlers::v2::engagement::get_poll,
        crate::handlers::v2::engagement::open_poll,
        crate::handlers::v2::engagement::close_poll,
        crate::handlers::v2::engagement::archive_poll,
        crate::handlers::v2::engagement::respond,
        crate::handlers::v2::engagement::results,
        crate::handlers::v2::engagement::submit_question,
        crate::handlers::v2::engagement::list_questions,
        crate::handlers::v2::engagement::vote,
        crate::handlers::v2::engagement::unvote,
        crate::handlers::v2::engagement::answer,
        crate::handlers::v2::engagement::dismiss,
        crate::handlers::v2::engagement::archive,
        crate::handlers::v2::engagement::pin,
        crate::handlers::v2::engagement::unpin,
        crate::handlers::v2::compat::shadow_compare,
        crate::handlers::v2::compat::run_backfill,
        crate::handlers::v2::unified_messaging::registration_challenge,
        crate::handlers::v2::unified_messaging::register_device,
        crate::handlers::v2::unified_messaging::publish_key_packages,
        crate::handlers::v2::unified_messaging::revoke_device,
        crate::handlers::v2::unified_messaging::client_token,
        crate::handlers::v2::unified_messaging::revoke_all_sessions,
        crate::handlers::v2::unified_messaging::me,
        crate::handlers::v2::unified_messaging::directory,
        crate::handlers::v2::unified_messaging::resolve,
        crate::handlers::v2::unified_messaging::principal_names,
        crate::handlers::v2::unified_messaging::relay_get,
        crate::handlers::v2::unified_messaging::relay_post,
        crate::handlers::v2::unified_messaging::relay_put,
        crate::handlers::v2::unified_messaging::relay_delete,
        crate::handlers::v2::unified_messaging::migration_status,
        crate::handlers::v2::unified_messaging::migration_transition,
        crate::handlers::v2::unified_messaging::migration_parity,
        crate::handlers::v2::spaces::set_recurrence_exception,
        crate::handlers::v2::spaces::list_recurrence_exceptions,
        crate::handlers::v2::organizations::create_organization,
        crate::handlers::v2::organizations::update_organization,
        crate::handlers::v2::organizations::list_organization_members,
        crate::handlers::v2::organizations::upsert_organization_member,
        crate::handlers::v2::organizations::my_organizations,
        // Parts G–I
        crate::handlers::v2::artifacts::start_recording,
        crate::handlers::v2::artifacts::list_recordings,
        crate::handlers::v2::artifacts::get_recording,
        crate::handlers::v2::artifacts::stop_recording,
        crate::handlers::v2::artifacts::list_artifacts,
        crate::handlers::v2::artifacts::get_artifact,
        crate::handlers::v2::artifacts::artifact_access,
        crate::handlers::v2::artifacts::delete_artifact,
        crate::handlers::v2::artifacts::generate_report,
        crate::handlers::v2::artifacts::conference_attendance,
        crate::handlers::v2::artifacts::consent_requirements,
        crate::handlers::v2::artifacts::record_consents,
        crate::handlers::v2::artifacts::withdraw_consent,
        crate::handlers::v2::artifacts::start_transcription,
        crate::handlers::v2::artifacts::stop_transcription,
        crate::handlers::v2::artifacts::list_transcripts,
        crate::handlers::v2::artifacts::get_transcript,
        crate::handlers::v2::artifacts::translate_transcript,
        crate::handlers::v2::artifacts::request_intelligence,
        crate::handlers::v2::specialized::get_profile_policy,
        crate::handlers::v2::specialized::update_profile_policy,
        crate::handlers::v2::specialized::register,
        crate::handlers::v2::specialized::list_registrations,
        crate::handlers::v2::specialized::get_registration,
        crate::handlers::v2::specialized::cancel_registration,
        crate::handlers::v2::specialized::enter_backstage,
        crate::handlers::v2::specialized::leave_backstage,
        crate::handlers::v2::quality::submit_samples,
        crate::handlers::v2::quality::conference_quality,
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
            crate::dto::ChangePasswordRequest,
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
            crate::dto::v2::ConferenceSummary,
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
            crate::dto::v2::JoinRealtimeInfo,
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
            crate::dto::v2::MuteRequest,
            crate::dto::v2::MediaLockResponse,
            crate::dto::v2::StopScreenShareResponse,
            crate::dto::v2::SpotlightRequest,
            crate::dto::v2::SpotlightView,
            crate::dto::v2::SpotlightListResponse,
            crate::dto::v2::ChangedResponse,
            crate::dto::v2::CollaborationPolicyUpdateRequest,
            crate::dto::v2::ChatPolicyUpdateRequest,
            crate::dto::v2::CollaborationPolicyResponse,
            crate::dto::v2::MyCollaborationState,
            crate::dto::v2::CollaborationStateResponse,
            crate::dto::v2::MediaCredentialRequest,
            crate::dto::v2::MediaCredentialResponse,
            crate::dto::v2::ReactionRequest,
            crate::dto::v2::ReactionResponse,
            crate::dto::v2::HandRaiseResponse,
            crate::dto::v2::RaisedHandsResponse,
            crate::dto::v2::MessageListQuery,
            crate::dto::v2::BreakoutListResponse,
            crate::dto::v2::BreakoutListCreatedResponse,
            crate::dto::v2::BreakoutResponse,
            crate::dto::v2::CloseBreakoutsResponse,
            crate::dto::v2::MediaContextResponse,
            crate::dto::v2::AnnouncementResponse,
            crate::dto::v2::PollListResponse,
            crate::dto::v2::PollTransitionResponse,
            crate::dto::v2::QuestionListResponse,
            crate::dto::v2::QuestionActionResponse,
            crate::dto::v2::AssignBreakoutsRequest,
            crate::dto::v2::AssignmentMode,
            crate::dto::v2::AssignmentPair,
            crate::dto::v2::AssignmentResult,
            crate::dto::v2::BreakoutAnnouncementRequest,
            crate::dto::v2::BreakoutStatus,
            crate::dto::v2::BreakoutView,
            crate::dto::v2::CloseBreakoutRequest,
            crate::dto::v2::CreateBreakoutsRequest,
            crate::dto::v2::ManualAssignment,
            crate::dto::v2::MediaContextView,
            crate::dto::v2::MoveParticipantRequest,
            crate::dto::v2::RoomAssignmentsPatch,
            crate::dto::v2::RaisedHand,
            crate::dto::v2::MessageKind,
            crate::dto::v2::MessagePage,
            crate::dto::v2::MessageView,
            crate::dto::v2::SendMessageRequest,
            crate::dto::v2::BreakoutRecordingPolicy,
            crate::dto::v2::ChatSendMode,
            crate::dto::v2::CollaborationPolicy,
            crate::dto::v2::ConsentKind,
            crate::dto::v2::PrivateChatMode,
            crate::dto::v2::ScreenSharePolicy,
            crate::dto::v2::CreatePollRequest,
            crate::dto::v2::PollAnswer,
            crate::dto::v2::PollAnswerInput,
            crate::dto::v2::PollOption,
            crate::dto::v2::PollQuestion,
            crate::dto::v2::PollQuestionInput,
            crate::dto::v2::PollQuestionType,
            crate::dto::v2::PollResponseReceipt,
            crate::dto::v2::PollResults,
            crate::dto::v2::PollStatus,
            crate::dto::v2::PollView,
            crate::dto::v2::QuestionResult,
            crate::dto::v2::ResultsVisibility,
            crate::dto::v2::SubmitPollResponseRequest,
            crate::dto::v2::TextAnswer,
            crate::dto::v2::ConsentGrant,
            crate::dto::v2::PreJoinAccess,
            crate::dto::v2::PreJoinConsents,
            crate::dto::v2::PreJoinDevices,
            crate::dto::v2::PreJoinDisplayName,
            crate::dto::v2::PreJoinMedia,
            crate::dto::v2::PreJoinNotices,
            crate::dto::v2::PreJoinOrganization,
            crate::dto::v2::PreJoinOrganizer,
            crate::dto::v2::PreJoinResponse,
            crate::dto::v2::AnswerQuestionRequest,
            crate::dto::v2::QuestionStatus,
            crate::dto::v2::QuestionView,
            crate::dto::v2::SubmitQuestionRequest,
            crate::services::collaboration::policy::CollaborationPolicyPatch,
            crate::dto::v2::MuteResponse,
            crate::dto::v2::RealtimeTicketResponse,
            crate::dto::v2::EventReplayResponse,
            crate::services::realtime::RealtimeEnvelope,
            crate::services::rtc_provider::TrackKind,
            crate::dto::v2::ResolveResponse,
            crate::dto::v2::ShadowComparisonResponse,
            crate::dto::v2::BackfillReportResponse,
            crate::services::unified_messaging::migration::ChatMigrationState,
            crate::services::unified_messaging::migration::TransitionRequest,
            crate::services::unified_messaging::migration::MigrationStatusView,
            crate::services::unified_messaging::migration::ParityRunView,
            crate::services::unified_messaging::migration::TransitionView,
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
            crate::error::ErrorEnvelope,
            crate::error::ErrorBody,
            crate::error::ErrorCode,
            crate::services::meeting_permissions::RolePolicy,
            crate::services::meeting_permissions::MediaPermissions,
            // Parts G–I
            crate::services::artifacts::ArtifactType,
            crate::services::artifacts::ArtifactStatus,
            crate::services::artifacts::consent::ConsentDecision,
            crate::services::artifacts::consent::ConsentRequirement,
            crate::services::artifacts::consent::ConsentRequirementsResponse,
            crate::services::artifacts::consent::ConsentDecisionInput,
            crate::services::artifacts::consent::RecordConsentRequest,
            crate::services::artifacts::consent::WithdrawalEffect,
            crate::services::artifacts::consent::WithdrawalResponse,
            crate::services::artifacts::intelligence::IntelligenceKind,
            crate::services::artifacts::intelligence::IntelligenceRequest,
            crate::services::artifacts::ArtifactView,
            crate::services::artifacts::recording::StartRecordingRequest,
            crate::services::artifacts::recording::ProviderDispatch,
            crate::services::artifacts::recording::RecordingResponse,
            crate::services::artifacts::service::ArtifactAccess,
            crate::services::artifacts::service::GenerateReportRequest,
            crate::services::artifacts::service::AttendanceReport,
            crate::services::artifacts::service::AttendanceEntry,
            crate::services::artifacts::transcription::TranscriptionModeDto,
            crate::services::artifacts::transcription::StartTranscriptionRequest,
            crate::services::artifacts::transcription::TranslateRequest,
            crate::services::artifacts::transcription::TranscriptionJobView,
            crate::services::artifacts::transcription::SegmentView,
            crate::services::artifacts::transcription::TranscriptView,
            crate::services::artifacts::transcription::TranslationResponse,
            crate::services::profiles::MeetingProfile,
            crate::services::profiles::ConsentMode,
            crate::services::profiles::WithdrawalMode,
            crate::services::profiles::ProcessingLocation,
            crate::services::profiles::FieldRequirement,
            crate::services::profiles::RecordingProfile,
            crate::services::profiles::TranscriptionProfile,
            crate::services::profiles::IntelligenceProfile,
            crate::services::profiles::DataSharingProfile,
            crate::services::profiles::ExportProfile,
            crate::services::profiles::AccessProfile,
            crate::services::profiles::AuditProfile,
            crate::services::profiles::StageProfile,
            crate::services::profiles::CustomRegistrationField,
            crate::services::profiles::RegistrationProfile,
            crate::services::profiles::RoleMappingProfile,
            crate::services::profiles::ProfilePolicy,
            crate::services::profiles::ResolvedProfile,
            crate::services::quality::service::MediaHealth,
            crate::services::quality::service::NetworkType,
            crate::services::quality::service::FailureKind,
            crate::services::quality::service::FailureCategoryDto,
            crate::services::quality::service::MediaFailure,
            crate::services::quality::service::QualitySampleInput,
            crate::services::quality::service::QualitySampleBatch,
            crate::services::quality::service::QualityIngestResponse,
            crate::services::quality::service::ConferenceQualityView,
            crate::services::specialized::ProfilePolicyUpdateRequest,
            crate::services::specialized::ProfilePolicyResponse,
            crate::services::specialized::RegistrationRequest,
            crate::services::specialized::RegistrationCreated,
            crate::services::specialized::RegistrationView,
            crate::services::specialized::BackstageResponse,
            crate::dto::v2_ghi::RecordingListResponse,
            crate::dto::v2_ghi::ArtifactListResponse,
            crate::dto::v2_ghi::TranscriptionJobListResponse,
            crate::dto::v2_ghi::RegistrationListResponse,
            // Internal provider-callback / operations schemas (`/internal/*`) are
            // deliberately NOT part of the public consumer contract.
        )
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "General", description = "Application information and health endpoints"),
        (name = "Authentication", description = "User authentication endpoints"),
        (name = "API Keys", description = "API key management for third-party integrations.\n\n## Usage\n- Create API keys via authenticated endpoints\n- Use `Api-Key: <key>` header for authentication\n- API keys have usage limits and expiration dates\n- Keys can be revoked at any time\n\n## Notes\n- Full API key is only shown on creation\n- Third-party devs cannot access internal media servers directly\n- Short-lived join tokens required for connecting to RTC infrastructure"),
        (name = "Meetings", description = "Meeting lifecycle management with LiveKit integration"),
        (name = "Participants", description = "Participant management (kick, list)"),
        (name = "Messaging (v2)", description = "Part V H: the signed-in user's Unified Messaging identity bridge (devices with client-held keys, short-lived device-bound client tokens, session revocation; Messaging's answer is relayed and no service credential is ever returned), the client Messaging API (the caller's principal and scope ids, organization colleagues, and an allowlist of end-user conversation, MLS Delivery Service, attachment and history-transfer operations relayed with the caller's principal signed) and the platform-admin chat migration controls (state machine, parity, rollback)."),
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
        (name = "Media controls (v2)", description = "Part F: provider-enforced media locks, host mute, screen-share stop, spotlight and credential refresh. Locks only reduce rights; grants are always the effective projection of role ∧ policy ∧ locks."),
        (name = "Collaboration (v2)", description = "Part F: collaboration policy, canonical collaboration state (REST recovery) and the pre-join contract."),
        (name = "Chat (v2)", description = "Part F: MeetingMessagingService — server-derived sender identity, main / breakout / private scope, cursor pagination, rate limits and moderation."),
        (name = "Engagement (v2)", description = "Part F: transient reactions and persistent hand raise."),
        (name = "Breakouts (v2)", description = "Part F: breakout rooms owned by the parent conference (provider room per breakout), assignment, routing, visits, announcements and countdown close."),
        (name = "Polls (v2)", description = "Part F: polls and quizzes with server-side scoring, anonymous ballots and results visibility."),
        (name = "Q&A (v2)", description = "Part F: questions, upvotes and moderation (separate from chat; anonymous identity never projected)."),
        (name = "Realtime (v2)", description = "Authorized conference event stream (WebSocket) and replay. PostgreSQL is authority; events are delivery only — recover canonical state from GET /api/v2/conferences/{id}."),
        (name = "Waiting Room (v2)", description = "Waiting is a participant admission state. Access policy is always evaluated first."),
        (name = "Organizations (v2)", description = "Authoritative CF organization membership used by ORGANIZATION_ONLY access. Membership is resolved server-side; clients can never assert it."),
        (name = "Compatibility (v2)", description = "Legacy → v2 migration evidence and shadow comparison (platform administrators)."),
        (name = "Recordings (v2)", description = "Part G: composite recording through the provider egress. REQUESTED → RECORDING only on provider evidence; STOPPING → PROCESSING → READY only after provider finalization and storage verification (size + checksum). One active composite recording per conference."),
        (name = "Artifacts (v2)", description = "Part G: recordings, transcripts and generated reports. Access is authorized per request and delivered as a short-lived signed URL; storage keys and provider ids are never exposed. Retention and deletion are confirmed against storage."),
        (name = "Consent (v2)", description = "Part G: independent consent per processing type (recording, transcription, intelligence, data sharing), explicit or notice mode per meeting profile, and withdrawal semantics."),
        (name = "Transcription (v2)", description = "Part G: provider-neutral transcription jobs, idempotent segment ingestion, WebVTT export and translation. Disabled unless a provider is configured and the profile permits it."),
        (name = "Intelligence (v2)", description = "Part G: bounded intelligence requests over READY transcripts, gated by profile, consent and processing location. Production summarization is not part of this release."),
        (name = "Profiles (v2)", description = "Part H: meeting profiles (STANDARD, WEBINAR, TOWN_HALL, HIGH_SECURITY, HEALTHCARE) resolved base → type → security → organization → space → conference with hard ceilings."),
        (name = "Registrations (v2)", description = "Part H: webinar / town-hall registration with policy-approved fields and one-time admission tokens."),
        (name = "Backstage (v2)", description = "Part H: isolated backstage media room for presenters and hosts."),
        (name = "Quality (v2)", description = "Part I: bounded participant quality samples (technical metrics only) and scoped conference quality for hosts."),
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

const DEFAULT_PUBLIC_SERVER_URL: &str = "https://api.cfmeet.com";

/// Public server URL advertised by Swagger: the configured value (trimmed, no trailing
/// slash) or the production default when unset / blank.
fn public_server_url(configured: Option<&str>) -> String {
    configured
        .map(|v| v.trim().trim_end_matches('/'))
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_PUBLIC_SERVER_URL)
        .to_string()
}

/// Contract-first OpenAPI documents for peer services. These are deliberately served by
/// the documentation host only; they do NOT proxy APIs or weaken the peer services' auth.
/// Swagger's dropdown can render the full platform without adding cross-service DB/code deps.
///
/// They are the BUNDLED renderings (`packages/contracts/bundled/**`, generated by
/// `scripts/contracts/bundle_openapi.py`): every `https://schemas.cfmeet.com/...` reference
/// is inlined into the document's own `components/schemas`. The schema host is a naming
/// authority, not a deployed service, so the raw contracts made Swagger UI report
/// "Could not resolve reference: Failed to fetch".
const CALLING_API: &str =
    include_str!("../../../../packages/contracts/bundled/calling/http/v1.openapi.json");
const PLATFORM_ACCESS_API: &str =
    include_str!("../../../../packages/contracts/bundled/platform-access/internal/v1.openapi.json");
const NUMBERING_API: &str =
    include_str!("../../../../packages/contracts/bundled/numbering/internal/v1.openapi.json");
const PRIVACY_RELAY_API: &str =
    include_str!("../../../../packages/contracts/bundled/privacy-relay/http/v1.openapi.json");
const PRIVACY_RELAY_INTERNAL_API: &str =
    include_str!("../../../../packages/contracts/bundled/privacy-relay/internal/v1.openapi.json");
const TELEPHONY_API: &str =
    include_str!("../../../../packages/contracts/bundled/telephony/internal/v1.openapi.json");
const SMS_GATEWAY_API: &str =
    include_str!("../../../../packages/contracts/bundled/sms-gateway/http/v1.openapi.json");
const MESSAGING_API: &str =
    include_str!("../../../../packages/contracts/bundled/messaging/http/v1.openapi.json");

/// (Swagger name, served path, document). The Meeting Service document is generated at
/// runtime (it carries the configured server list) and is listed first.
const PEER_DOCS: [(&str, &str, &str); 8] = [
    ("Calling", "/api-docs/calling.json", CALLING_API),
    (
        "Platform Access (internal)",
        "/api-docs/platform-access.json",
        PLATFORM_ACCESS_API,
    ),
    (
        "Numbering / DCNs",
        "/api-docs/numbering.json",
        NUMBERING_API,
    ),
    (
        "Privacy Relay",
        "/api-docs/privacy-relay.json",
        PRIVACY_RELAY_API,
    ),
    (
        "Privacy Relay (internal)",
        "/api-docs/privacy-relay-internal.json",
        PRIVACY_RELAY_INTERNAL_API,
    ),
    (
        "Telephony (internal)",
        "/api-docs/telephony.json",
        TELEPHONY_API,
    ),
    ("SMS Gateway", "/api-docs/sms-gateway.json", SMS_GATEWAY_API),
    ("Messaging", "/api-docs/messaging.json", MESSAGING_API),
];

/// swagger-ui-dist, pinned by version AND by Subresource Integrity (sha384 of the exact
/// published files), so a compromised CDN cannot substitute the script. This replaces the
/// `utoipa-swagger-ui` crate, whose build script downloads and extracts a ZIP with a
/// `zip` release affected by RUSTSEC-2026-0367 / 0368 / 0369 (no fixed `zip` is used by
/// any `utoipa-swagger-ui` release).
const SWAGGER_UI_VERSION: &str = "5.17.14";
const SWAGGER_UI_CSS_SRI: &str =
    "sha384-wxLW6kwyHktdDGr6Pv1zgm/VGJh99lfUbzSn6HNHBENZlCN7W602k9VkGdxuFvPn";
const SWAGGER_UI_JS_SRI: &str =
    "sha384-wmyclcVGX/WhUkdkATwhaK1X1JtiNrr2EoYJ+diV3vj4v6OC5yCeSu+yW13SYJep";
/// The page may load only the pinned assets; documents come from this origin only.
const SWAGGER_UI_CSP: &str =
    "default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-SWAGGER_INIT'; \
     style-src https://cdn.jsdelivr.net 'unsafe-inline'; img-src 'self' data:; \
     connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

const SWAGGER_INIT: &str = r#"
window.addEventListener('load', function() {
  var selector = document.getElementById('swagger-service');
  var controls = document.getElementById('swagger-controls');
  function render(url) {
    window.ui = SwaggerUIBundle({
      url: url,
      dom_id: '#swagger-ui',
      deepLinking: true,
      displayRequestDuration: true,
      filter: true,
      supportedSubmitMethods: [],
      validatorUrl: null
    });
  }
  fetch('/api-docs/swagger-config.json', {cache:'no-store'})
    .then(function(response) {
      if (!response.ok) throw new Error('Swagger config unavailable');
      return response.json();
    })
    .then(function(config) {
      var docs = Array.isArray(config.urls) ? config.urls.filter(function(d) {
        return d && typeof d.url === 'string' && d.url.startsWith('/api-docs/')
          && typeof d.name === 'string';
      }) : [];
      if (!docs.length) { render('/api-docs/openapi.json'); return; }
      docs.forEach(function(d) {
        var option = document.createElement('option');
        option.value = d.url;
        option.textContent = d.name;
        selector.appendChild(option);
      });
      var primary = docs.find(function(d) { return d.name === config.primaryName; }) || docs[0];
      selector.value = primary.url;
      selector.addEventListener('change', function() { render(selector.value); });
      controls.hidden = false;
      render(primary.url);
    })
    .catch(function() { render('/api-docs/openapi.json'); });
});
"#;

fn swagger_page() -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>CF Meet API documentation</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{v}/swagger-ui.css" integrity="{css}" crossorigin="anonymous" referrerpolicy="no-referrer" />
</head>
<body>
  <div id="swagger-controls" hidden style="padding:12px 20px;font:14px sans-serif"><label for="swagger-service">API service: </label><select id="swagger-service" aria-label="Select API service"></select></div>
  <div id="swagger-ui"></div>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@{v}/swagger-ui-bundle.js" integrity="{js}" crossorigin="anonymous" referrerpolicy="no-referrer"></script>
  <script>{init}</script>
</body>
</html>
"#,
        v = SWAGGER_UI_VERSION,
        css = SWAGGER_UI_CSS_SRI,
        js = SWAGGER_UI_JS_SRI,
        init = SWAGGER_INIT,
    )
}

fn swagger_csp() -> String {
    use base64::Engine;
    use sha2::Digest;
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(SWAGGER_INIT));
    SWAGGER_UI_CSP.replace("SWAGGER_INIT", &hash)
}

fn swagger_config() -> serde_json::Value {
    let mut urls = vec![serde_json::json!({ "name": "Meetings", "url": "/api-docs/openapi.json" })];
    urls.extend(
        PEER_DOCS
            .iter()
            .map(|(name, path, _)| serde_json::json!({ "name": name, "url": path })),
    );
    serde_json::json!({ "urls": urls, "primaryName": "Meetings" })
}

fn contract_doc(body: &'static str) -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
}

/// Creates the CF Meet Swagger UI with a service selector. The Meeting Service remains
/// the default and keeps its original /api-docs/openapi.json contract unchanged.
/// Peer service documents come from the bundled renderings of the checked-in contracts.
pub fn swagger_router(server_addr: &str) -> Router {
    let mut openapi = ApiDoc::openapi();

    // Meeting Service Try It Out continues to use its configured deployment URL.
    let mut current_server = utoipa::openapi::Server::new(format!("http://{}", server_addr));
    current_server.description = Some("Current Meeting Service".to_string());
    let configured = std::env::var("SWAGGER_SERVER_URL").ok();
    let mut public_server = utoipa::openapi::Server::new(public_server_url(configured.as_deref()));
    public_server.description = Some("Meeting Service deployment".to_string());
    openapi.servers = Some(vec![current_server, public_server]);
    let meeting_doc = openapi.to_json().expect("OpenAPI document serializes");

    // Internal specifications are *documentation*, not new externally executable routes.
    // Trying another service's operations requires that service's own reachable ingress,
    // authentication and environment-specific server configuration.
    let page = swagger_page();
    let csp = swagger_csp();
    let config = swagger_config().to_string();
    let mut router = Router::new()
        .route(
            "/swagger-ui",
            get(|| async { Redirect::permanent("/swagger-ui/") }),
        )
        .route(
            "/swagger-ui/",
            get(move || {
                let page = page.clone();
                let csp = csp.clone();
                async move {
                    (
                        [
                            (CONTENT_SECURITY_POLICY, csp),
                            (CACHE_CONTROL, "no-store, max-age=0".to_string()),
                        ],
                        Html(page),
                    )
                }
            }),
        )
        .route(
            "/api-docs/swagger-config.json",
            get(move || {
                let config = config.clone();
                async move { ([(CONTENT_TYPE, "application/json")], config) }
            }),
        )
        .route(
            "/api-docs/openapi.json",
            get(move || {
                let doc = meeting_doc.clone();
                async move { ([(CONTENT_TYPE, "application/json")], doc) }
            }),
        );
    for (_, path, body) in PEER_DOCS {
        router = router.route(path, get(move || async move { contract_doc(body) }));
    }
    router
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_openapi_specs_are_valid_and_expose_expected_operations() {
        let specs = [
            (CALLING_API, "/v1/calls"),
            (PLATFORM_ACCESS_API, "/internal/v1"),
            (NUMBERING_API, "/v1/numbers/assignments"),
            (PRIVACY_RELAY_API, "/v1/privacy-relay/contexts"),
            (PRIVACY_RELAY_INTERNAL_API, "/internal/v1"),
            (TELEPHONY_API, "/internal/v1"),
            (SMS_GATEWAY_API, "/v1/sms/messages"),
        ];
        for (raw, expected_prefix) in specs {
            let doc: serde_json::Value = serde_json::from_str(raw).expect("valid OpenAPI JSON");
            assert_eq!(doc["openapi"], "3.1.0");
            let paths = doc["paths"].as_object().expect("OpenAPI paths");
            assert!(
                paths.keys().any(|path| path.starts_with(expected_prefix)),
                "missing {expected_prefix} in {}",
                doc["info"]["title"]
            );
        }
    }

    #[tokio::test]
    async fn swagger_serves_all_peer_contracts_from_the_doc_host() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;

        let router = swagger_router("127.0.0.1:18081");
        for path in [
            "/api-docs/openapi.json",
            "/api-docs/calling.json",
            "/api-docs/platform-access.json",
            "/api-docs/numbering.json",
            "/api-docs/privacy-relay.json",
            "/api-docs/privacy-relay-internal.json",
            "/api-docs/telephony.json",
            "/api-docs/sms-gateway.json",
            "/api-docs/messaging.json",
            "/api-docs/swagger-config.json",
            "/swagger-ui/",
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
    }

    /// Every `$ref` of every document Swagger renders resolves INSIDE that document: no
    /// external host is needed (the "Could not resolve reference: Failed to fetch" defect).
    #[test]
    fn every_served_document_resolves_all_references_locally() {
        fn resolve<'a>(doc: &'a serde_json::Value, pointer: &str) -> Option<&'a serde_json::Value> {
            doc.pointer(pointer)
        }
        fn walk(doc: &serde_json::Value, node: &serde_json::Value, name: &str, refs: &mut usize) {
            match node {
                serde_json::Value::Array(items) => {
                    items.iter().for_each(|i| walk(doc, i, name, refs))
                }
                serde_json::Value::Object(map) => {
                    if let Some(r) = map.get("$ref").and_then(|r| r.as_str()) {
                        *refs += 1;
                        let pointer = r
                            .strip_prefix('#')
                            .unwrap_or_else(|| panic!("{name}: external reference {r}"));
                        assert!(
                            resolve(doc, pointer).is_some(),
                            "{name}: {r} does not resolve"
                        );
                    }
                    map.values().for_each(|v| walk(doc, v, name, refs));
                }
                _ => {}
            }
        }
        let meeting = openapi_json();
        let mut docs = vec![("Meetings", meeting.as_str())];
        docs.extend(PEER_DOCS.iter().map(|(n, _, b)| (*n, *b)));
        for (name, raw) in docs {
            let doc: serde_json::Value = serde_json::from_str(raw).expect("valid JSON");
            assert!(doc["openapi"].as_str().unwrap().starts_with("3."), "{name}");
            // (Descriptions may still NAME a schema URL; only `$ref`s must be local.)
            assert!(!raw.contains("\"$ref\": \"https://"), "{name}");
            let mut refs = 0;
            walk(&doc, &doc, name, &mut refs);
            if name != "Meetings" {
                assert!(refs > 0, "{name}: expected bundled references");
            }
        }
    }

    #[test]
    fn swagger_page_pins_its_assets_and_sets_a_restrictive_csp() {
        let page = swagger_page();
        assert_eq!(
            page.matches("integrity=\"sha384-").count(),
            2,
            "SRI on CSS and JS"
        );
        assert!(page.contains(&format!("swagger-ui-dist@{SWAGGER_UI_VERSION}/")));
        assert!(page.contains(r#"id="swagger-service""#));
        assert!(SWAGGER_INIT.contains("supportedSubmitMethods: []"));
        assert!(SWAGGER_INIT.contains("url: url"));
        assert!(SWAGGER_INIT.contains("selector.addEventListener"));
        assert!(!SWAGGER_INIT.contains("urls:c.urls"));
        let csp = swagger_csp();
        assert!(csp.contains("default-src 'none'") && csp.contains("connect-src 'self'"));
        assert!(!csp.contains("SWAGGER_INIT") && !csp.contains("'unsafe-eval'"));
        let config = swagger_config();
        assert_eq!(
            config["urls"].as_array().unwrap().len(),
            PEER_DOCS.len() + 1
        );
    }

    #[test]
    fn public_server_url_defaults_to_production() {
        assert_eq!(public_server_url(None), DEFAULT_PUBLIC_SERVER_URL);
        assert_eq!(public_server_url(Some("  ")), DEFAULT_PUBLIC_SERVER_URL);
    }

    #[test]
    fn public_server_url_uses_configured_value() {
        assert_eq!(
            public_server_url(Some(" https://api.staging.cfmeet.com/ ")),
            "https://api.staging.cfmeet.com"
        );
    }
}
