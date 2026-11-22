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
        crate::handlers::v2::account_phone::request_signin,
        crate::handlers::v2::account_phone::verify_signin,
        crate::handlers::v2::account_phone::get_my_phone,
        crate::handlers::v2::account_phone::request_binding,
        crate::handlers::v2::account_phone::confirm_binding,
        crate::handlers::v2::account_phone::update_privacy,
        crate::handlers::v2::account_phone::remove_phone,
        crate::handlers::auth::request_otp,
        crate::handlers::auth::verify_otp,
        crate::handlers::auth::verify_otp_link,
        crate::handlers::auth::verify_mfa,
        crate::handlers::auth::oauth_google_native,
        crate::handlers::auth::refresh_token,
        crate::handlers::auth::logout,
        crate::handlers::auth::revert_email_change,
        // OAuth endpoints
        crate::handlers::auth::oauth_google_init,
        crate::handlers::auth::oauth_google_callback,
        crate::handlers::auth::oauth_google_exchange,
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
        crate::handlers::admin::change_user_email,
        crate::handlers::admin::list_meetings,
        crate::handlers::admin::list_logs,
        // Phase 2 meeting domain (/api/v2)
        // Document 4 §3.1: Notification Service user APIs (relay)
        crate::handlers::v2::notifications::list_my_notifications,
        crate::handlers::v2::notifications::get_my_notification_unread_count,
        crate::handlers::v2::notifications::update_my_notification,
        crate::handlers::v2::notifications::mark_my_notifications_read,
        crate::handlers::v2::notifications::dismiss_my_notifications,
        crate::handlers::v2::notifications::get_my_notification_preferences,
        crate::handlers::v2::notifications::update_my_notification_preferences,
        crate::handlers::v2::notifications::register_my_push_endpoint,
        crate::handlers::v2::notifications::remove_my_push_endpoint,
        // ADR 0064 / 0065: CF Meet Apps
        crate::handlers::v2::apps::authorize_context,
        crate::handlers::v2::apps::authorize_decide,
        crate::handlers::v2::apps::list_my_apps,
        crate::handlers::v2::apps::revoke_my_app,
        crate::handlers::v2::apps::list_organization_apps,
        crate::handlers::v2::apps::revoke_organization_app,
        crate::handlers::v2::apps::list_conference_apps,
        crate::handlers::v2::apps::create_app_context,
        crate::handlers::v2::apps::app_context_keys,
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
        crate::handlers::v2::members::add_team_members,
        crate::handlers::v2::members::update_member,
        crate::handlers::v2::members::revoke_member,
        crate::handlers::v2::invitations::create_invitation,
        crate::handlers::v2::invitations::list_invitations,
        crate::handlers::v2::invitations::revoke_invitation,
        crate::handlers::v2::invitations::resolve_invitation,
        crate::handlers::v2::conferences::create_conference,
        crate::handlers::v2::conferences::list_conferences,
        crate::handlers::v2::conferences::get_conference,
        crate::handlers::v2::calendar::my_conferences,
        crate::handlers::v2::calendar::conference_calendar,
        crate::handlers::v2::calendar::meeting_space_calendar,
        crate::handlers::v2::conferences::prepare_conference,
        crate::handlers::v2::conferences::start_conference,
        crate::handlers::v2::conferences::lock_conference,
        crate::handlers::v2::conferences::unlock_conference,
        crate::handlers::v2::conferences::end_conference,
        crate::handlers::v2::conferences::cancel_conference,
        crate::handlers::v2::conferences::reschedule_conference,
        crate::handlers::v2::conferences::extend_conference,
        crate::handlers::v2::calendar::get_calendar_feed,
        crate::handlers::v2::calendar::create_calendar_feed,
        crate::handlers::v2::calendar::delete_calendar_feed,
        crate::handlers::v2::calendar::read_calendar_feed,
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
        crate::handlers::v2::collaboration::edit_message,
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
        crate::handlers::v2::engagement::restore,
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
        crate::handlers::v2::unified_messaging::session_token,
        crate::handlers::v2::unified_messaging::revoke_all_sessions,
        crate::handlers::v2::unified_messaging::me,
        crate::handlers::v2::unified_messaging::directory,
        crate::handlers::v2::unified_messaging::resolve,
        crate::handlers::v2::unified_messaging::principal_names,
        crate::handlers::v2::unified_messaging::relay_get,
        crate::handlers::v2::unified_messaging::relay_post,
        crate::handlers::v2::unified_messaging::relay_put,
        crate::handlers::v2::unified_messaging::relay_patch,
        crate::handlers::v2::unified_messaging::relay_delete,
        crate::handlers::v2::unified_messaging::migration_status,
        crate::handlers::v2::unified_messaging::migration_transition,
        crate::handlers::v2::unified_messaging::migration_parity,
        crate::handlers::v2::spaces::set_recurrence_exception,
        crate::handlers::v2::spaces::list_recurrence_exceptions,
        crate::handlers::v2::organizations::create_organization,
        crate::handlers::v2::organizations::update_organization,
        crate::handlers::v2::organizations::list_organization_members,
        crate::handlers::v2::organizations::list_organization_meeting_spaces,
        crate::handlers::v2::organizations::export_organization,
        crate::handlers::v2::organizations::get_organization_meeting_profile_policy,
        crate::handlers::v2::organizations::put_organization_meeting_profile_policy,
        crate::handlers::v2::organizations::upsert_organization_member,
        crate::handlers::v2::organizations::my_organizations,
        crate::handlers::v2::organizations::create_business_organization,
        crate::handlers::v2::organizations::get_business_organization,
        crate::handlers::v2::organizations::update_business_organization,
        crate::handlers::v2::organizations::update_organization_membership,
        crate::handlers::v2::organizations::leave_organization,
        crate::handlers::v2::organizations::transfer_organization_ownership,
        crate::handlers::v2::organizations::create_organization_invitation,
        crate::handlers::v2::organizations::list_organization_invitations,
        crate::handlers::v2::organizations::revoke_organization_invitation,
        crate::handlers::v2::organizations::accept_organization_invitation,
        crate::handlers::v2::organizations::my_organization_invitations,
        crate::handlers::v2::organizations::appoint_organization_owner,
        crate::handlers::v2::organizations::admin_retry_organization_provisioning,
        crate::handlers::v2::organizations::admin_get_organization_service_binding,
        crate::handlers::v2::organizations::admin_rotate_organization_service_binding,
        crate::handlers::v2::organizations::admin_organization_event_delivery,
        crate::handlers::v2::organizations::admin_replay_organization_events,
        crate::handlers::v2::organizations::accept_my_organization_invitation,
        crate::handlers::v2::organizations::decline_my_organization_invitation,
        crate::handlers::v2::organizations::decline_organization_invitation,
        crate::handlers::v2::organizations::suspend_business_organization,
        crate::handlers::v2::organizations::reactivate_business_organization,
        crate::handlers::v2::organizations::archive_business_organization,
        crate::handlers::v2::organizations::list_organization_audit_events,
        crate::handlers::v2::organizations::get_organization_provisioning,
        crate::handlers::v2::organizations::admin_list_organizations,
        crate::handlers::v2::organizations::list_organization_teams,
        crate::handlers::v2::organizations::create_organization_team,
        crate::handlers::v2::organizations::get_organization_team,
        crate::handlers::v2::organizations::update_organization_team,
        crate::handlers::v2::organizations::archive_organization_team,
        crate::handlers::v2::organizations::list_organization_team_members,
        crate::handlers::v2::organizations::add_organization_team_member,
        crate::handlers::v2::organizations::update_organization_team_member,
        crate::handlers::v2::organizations::remove_organization_team_member,
        crate::handlers::v2::organizations::get_organization_policy,
        crate::handlers::v2::organizations::update_organization_policy,
        crate::handlers::v2::workspaces::my_workspaces,
        crate::handlers::v2::workspaces::my_workspace_capabilities,
        crate::handlers::v2::organization_calling::calling_get,
        crate::handlers::v2::organization_calling::calling_post,
        crate::handlers::v2::organization_calling::calling_put,
        crate::handlers::v2::organization_calling::calling_patch,
        crate::handlers::v2::organization_calling::calling_delete,
        crate::handlers::v2::organization_messaging::messaging_get,
        crate::handlers::v2::organization_messaging::messaging_post,
        crate::handlers::v2::organization_messaging::messaging_put,
        crate::handlers::v2::organization_messaging::messaging_patch,
        crate::handlers::v2::organization_messaging::messaging_delete,
        crate::handlers::v2::organization_messaging::messaging_session_token,
        crate::handlers::v2::organization_messaging::moderation_get,
        crate::handlers::v2::organization_messaging::moderation_post,
        crate::handlers::v2::organization_messaging::team_channel,
        crate::handlers::v2::organization_messaging::messaging_resolve_members,
        crate::handlers::v2::organization_member_calling::endpoints_list,
        crate::handlers::v2::organization_member_calling::endpoints_register,
        crate::handlers::v2::organization_member_calling::endpoints_revoke,
        crate::handlers::v2::organization_member_calling::signaling_credential,
        crate::handlers::v2::account_profile::get_my_profile,
        crate::handlers::v2::account_profile::update_my_profile,
        crate::handlers::v2::account_profile::upload_my_avatar,
        crate::handlers::v2::account_profile::remove_my_avatar,
        crate::handlers::v2::account_profile::get_avatar,
        crate::handlers::v2::account_sessions::list_my_sessions,
        crate::handlers::v2::account_sessions::revoke_my_session,
        crate::handlers::v2::account_sessions::revoke_all_my_sessions,
        crate::handlers::v2::account_sessions::revoke_other_sessions,
        crate::handlers::v2::account_sessions::admin_revoke_user_sessions,
        crate::handlers::v2::account_sessions::admin_identity_duplicates,
        crate::handlers::v2::account_sessions::admin_enforce_identity_uniqueness,
        crate::handlers::v2::account_sessions::list_account_audit_events,
        crate::handlers::v2::account_identity::request_step_up,
        crate::handlers::v2::account_identity::verify_step_up,
        crate::handlers::v2::account_identity::export_my_data,
        crate::handlers::v2::account_identity::delete_my_account,
        crate::handlers::v2::account_identity::get_my_mfa,
        crate::handlers::v2::account_identity::setup_my_totp,
        crate::handlers::v2::account_identity::confirm_my_totp,
        crate::handlers::v2::account_identity::disable_my_mfa,
        crate::handlers::v2::account_identity::request_email_change,
        crate::handlers::v2::account_identity::confirm_email_change,
        crate::handlers::v2::account_devices::get_device_policy,
        crate::handlers::v2::account_devices::list_devices,
        crate::handlers::v2::account_devices::declare_device,
        crate::handlers::v2::account_devices::revoke_device,
        crate::handlers::v2::account_devices::create_link,
        crate::handlers::v2::account_devices::list_pending_links,
        crate::handlers::v2::account_devices::get_link,
        crate::handlers::v2::account_devices::approve_link,
        crate::handlers::v2::account_devices::reject_link,
        crate::handlers::v2::account_devices::send_email_code,
        crate::handlers::v2::account_devices::complete_link,
        crate::handlers::v2::account_devices::cancel_link,
        crate::handlers::v2::account_devices::list_my_security_events,
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
        crate::handlers::v2::artifacts::export_report,
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
        crate::handlers::v2::specialized::cancel_own_registration,
        crate::handlers::v2::specialized::enter_backstage,
        crate::handlers::v2::specialized::leave_backstage,
        crate::handlers::v2::quality::submit_samples,
        crate::handlers::v2::quality::conference_quality,
    ),
    components(
        schemas(
            crate::services::account_phone::PhoneView,
            crate::services::account_phone::PhoneRequest,
            crate::services::account_phone::PhoneConfirm,
            crate::services::account_phone::PhoneChallenge,
            crate::handlers::v2::account_phone::PhoneBindRequest,
            crate::handlers::v2::account_phone::PhoneSettings,
            crate::handlers::v2::account_phone::PhoneRemove,
            crate::handlers::v2::specialized::CancelOwnRegistrationRequest,
            // Auth schemas
            crate::dto::OtpRequest,
            crate::dto::OtpVerifyRequest,
            crate::dto::OtpVerifyLinkRequest,
            crate::handlers::auth::MfaVerifyRequest,
            crate::handlers::auth::GoogleNativeRequest,
            crate::dto::OtpVerifyResponse,
            crate::dto::EmailChangeRevertRequest,
            crate::dto::LoginResponse,
            crate::dto::UserInfo,
            crate::dto::RefreshTokenRequest,
            crate::dto::RefreshTokenResponse,
            crate::dto::MessageResponse,
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
            crate::dto::DeactivateAccountRequest,
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
            crate::dto::AdminChangeEmailRequest,
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
            crate::dto::v2::OrganizationMeetingSpace,
            crate::dto::v2::OrganizationMeetingSpacePage,
            crate::dto::v2::OrganizationMeetingProfilePolicy,
            crate::dto::v2::PutOrganizationMeetingProfilePolicyRequest,
            crate::dto::v2::InstantMeetingResponse,
            crate::dto::v2::AddMemberRequest,
            crate::dto::v2::AddTeamMembersRequest,
            crate::dto::v2::AddTeamMembersResponse,
            crate::dto::v2::UpdateMemberRequest,
            crate::dto::v2::MemberResponse,
            crate::dto::v2::MemberListResponse,
            crate::dto::v2::CreateInvitationRequest,
            crate::dto::v2::InvitationResponse,
            crate::dto::v2::CreateInvitationResponse,
            crate::dto::v2::ResolveInvitationResponse,
            crate::dto::v2::CreateConferenceRequest,
            crate::dto::v2::RescheduleConferenceRequest,
            crate::dto::v2::ExtendConferenceRequest,
            crate::dto::v2::CalendarFeedResponse,
            crate::dto::v2::ListConferencesQuery,
            crate::dto::v2::ConferenceSummary,
            crate::dto::v2::ConferenceResponse,
            crate::dto::v2::ConferenceListResponse,
            crate::dto::v2::ListMeetingSpacesQuery,
            crate::dto::v2::AgendaQuery,
            crate::dto::v2::AgendaRelation,
            crate::dto::v2::AgendaSpace,
            crate::dto::v2::AgendaItem,
            crate::dto::v2::AgendaResponse,
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
            crate::dto::v2::EditMessageRequest,
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
            crate::dto::v2::OrganizationLegalRegistration,
            crate::dto::v2::OrganizationProfile,
            crate::dto::v2::CreateBusinessOrganizationRequest,
            crate::dto::v2::UpdateBusinessOrganizationRequest,
            crate::dto::v2::OrganizationViewer,
            crate::dto::v2::BusinessOrganization,
            crate::dto::v2::BusinessOrganizationPage,
            crate::dto::v2::BusinessOrganizationMembership,
            crate::dto::v2::UpdateBusinessMembershipRequest,
            crate::dto::v2::OrganizationOwnershipTransferRequest,
            crate::dto::v2::CreateOrganizationInvitationRequest,
            crate::dto::v2::OrganizationInvitation,
            crate::dto::v2::OrganizationInvitationTokenRequest,
            crate::dto::v2::OrganizationLifecycleRequest,
            crate::dto::v2::OrganizationInvitationList,
            crate::dto::v2::MyOrganizationInvitationList,
            crate::dto::v2::AppointOrganizationOwnerRequest,
            crate::handlers::v2::organizations::ReplayOrganizationEventsRequest,
            crate::dto::v2::MyOrganizationInvitation,
            crate::dto::v2::InvitingOrganization,
            crate::dto::v2::OrganizationAuditActor,
            crate::dto::v2::OrganizationAuditEvent,
            crate::dto::v2::OrganizationAuditEventPage,
            crate::dto::v2::OrganizationScopeMapping,
            crate::dto::v2::OrganizationProvisioning,
            crate::dto::v2::OrganizationTeam,
            crate::dto::v2::OrganizationTeamPage,
            crate::dto::v2::CreateOrganizationTeamRequest,
            crate::dto::v2::UpdateOrganizationTeamRequest,
            crate::dto::v2::ArchiveOrganizationTeamRequest,
            crate::dto::v2::OrganizationTeamMember,
            crate::dto::v2::OrganizationTeamMemberPage,
            crate::dto::v2::AddOrganizationTeamMemberRequest,
            crate::dto::v2::UpdateOrganizationTeamMemberRequest,
            crate::dto::v2::OrganizationPolicyMemberDirectory,
            crate::dto::v2::OrganizationPolicyGuests,
            crate::dto::v2::OrganizationPolicyMeetings,
            crate::dto::v2::OrganizationPolicyFeatures,
            crate::dto::v2::OrganizationPolicyRetention,
            crate::dto::v2::OrganizationPolicyDelegatedAdministration,
            crate::dto::v2::OrganizationPolicyDocument,
            crate::dto::v2::OrganizationPolicy,
            crate::dto::v2::UpdateOrganizationPolicyRequest,
            crate::dto::v2::WorkspaceKind,
            crate::dto::v2::WorkspaceAuthority,
            crate::dto::v2::WorkspaceCapabilityReason,
            crate::dto::v2::WorkspaceCapabilitySummary,
            crate::dto::v2::Workspace,
            crate::dto::v2::WorkspacePage,
            crate::dto::v2::WorkspaceCapability,
            crate::dto::v2::WorkspaceFeatures,
            crate::dto::v2::WorkspaceCapabilities,
            crate::dto::auth::GoogleExchangeRequest,
            crate::services::account_security::SessionRecord,
            crate::services::account_security::SecurityEventRecord,
            crate::services::account_profile::AccountProfile,
            crate::services::notifications::EmailDelivery,
            crate::services::account_profile::PhoneVerificationStatus,
            crate::handlers::v2::account_profile::UpdateAccountProfileRequest,
            crate::handlers::v2::account_sessions::AccountSessionListResponse,
            crate::handlers::v2::account_sessions::AccountSessionRevokeResponse,
            crate::handlers::v2::account_sessions::AdminRevokeSessionsRequest,
            crate::services::account_security::IdentityDuplicatesReport,
            crate::services::account_security::IdentityUniqueness,
            crate::handlers::v2::account_sessions::SecurityEventPage,
            crate::handlers::v2::account_identity::StepUpRequest,
            crate::handlers::v2::account_identity::DeleteAccountRequest,
            crate::handlers::v2::account_identity::MfaSetupResponse,
            crate::handlers::v2::account_identity::MfaConfirmRequest,
            crate::handlers::v2::account_identity::MfaRecoveryCodes,
            crate::handlers::v2::account_identity::MfaDisableRequest,
            crate::handlers::v2::account_identity::StepUpVerifyRequest,
            crate::handlers::v2::account_identity::CodeSentResponse,
            crate::handlers::v2::account_identity::StepUpTokenResponse,
            crate::handlers::v2::account_identity::EmailChangeRequest,
            crate::handlers::v2::account_identity::EmailChangeConfirmRequest,
            crate::handlers::v2::account_identity::EmailChangedResponse,
            crate::services::step_up::StepUpPurpose,
            crate::handlers::v2::account_devices::DeviceListResponse,
            crate::handlers::v2::account_devices::PendingLinkListResponse,
            crate::handlers::v2::account_devices::RevokeDeviceRequest,
            crate::handlers::v2::account_devices::MySecurityEventPage,
            crate::services::account_devices::DeviceProof,
            crate::services::account_devices::DeclareDeviceRequest,
            crate::services::account_devices::DeviceView,
            crate::services::account_devices::DevicePolicyView,
            crate::services::account_devices::DeviceLinkView,
            crate::services::account_devices::PendingLinkView,
            crate::services::account_devices::DeviceOutcome,
            crate::services::account_devices::ProofOnlyRequest,
            crate::services::account_devices::ApproveLinkRequest,
            crate::services::account_devices::CompleteLinkRequest,
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
            crate::validation::ValidationIssue,
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
            crate::dto::apps::AuthorizeDecisionRequest,
            crate::dto::apps::AuthorizeContextResponse,
            crate::dto::apps::AuthorizeDecisionResponse,
            crate::dto::apps::OauthAppSummary,
            crate::dto::apps::OauthScopeView,
            crate::dto::apps::OrganizationInstallDecision,
            crate::dto::apps::ConnectedApp,
            crate::dto::apps::ConnectedAppList,
            crate::dto::apps::MeetingSurfaceView,
            crate::dto::apps::ConferenceApp,
            crate::dto::apps::ConferenceAppList,
            crate::dto::apps::AppContextTokenResponse,
            crate::dto::apps::AppContextJwk,
            crate::dto::apps::AppContextKeySet,
            // Internal provider-callback / operations schemas (`/internal/*`) are
            // deliberately NOT part of the public consumer contract.
        )
    ),
    modifiers(&SecurityAddon, &PlatformCredentialAddon, &OptionalBodyAddon),
    tags(
        (name = "General", description = "Application information and health endpoints"),
        (name = "Authentication", description = "Passwordless sign-in and sign-up: request a 6-digit code by email (`POST /api/v1/auth/otp/request`), then verify the code or the one-click link from the same email. The first verification of a new address creates the account. Also token refresh, logout, Google sign-in and undoing an email change."),
        (name = "Account profile (v2)", description = "Blueprint Section 9: the signed-in user's account profile (username, display name, avatar reference, time zone, locale, email verification and the canonical account status). The Account is the authority; the Communications profile shows the same display name and avatar."),
        (name = "Account security (v2)", description = "Step-up confirmation (a code emailed to the account's current address) for sensitive actions, and the secure email change: proof of the current mailbox, a code to the new one, other sessions signed out, and a 7-day undo link sent to the previous address."),
        (name = "Account sessions (v2)", description = "Blueprint Section 2: the signed-in user's account sessions (one per sign-in, 30-day absolute lifetime, rotating single-use refresh tokens), per-session and everywhere sign-out, and the platform-administrator read of the append-only account security audit."),
        (name = "Account devices (v2)", description = "Multi-device policy: one account identity with one replaceable primary mobile and up to `maxWebDevices` web devices (browser profiles); web-only accounts need no phone. Devices are bound to device-generated Ed25519 keys (proof of possession, single-use nonces). New devices are linked by approval from a trusted device (comparison code) or an email step-up; a new mobile replaces the previous primary mobile atomically without signing out trusted web devices."),
        (name = "API Keys", description = "API key management for third-party integrations.\n\n## Usage\n- Create API keys via authenticated endpoints\n- Use `Api-Key: <key>` header for authentication\n- API keys have usage limits and expiration dates\n- Keys can be revoked at any time\n\n## Notes\n- Full API key is only shown on creation\n- Third-party devs cannot access internal media servers directly\n- Short-lived join tokens required for connecting to RTC infrastructure"),
        (name = "Meetings", description = "Meeting lifecycle management with LiveKit integration"),
        (name = "Participants", description = "Participant management (kick, list)"),
        (name = "Messaging (v2)", description = "Part V H: the signed-in user's Unified Messaging identity bridge (devices with client-held keys, short-lived device-bound client tokens, session revocation; Messaging's answer is relayed and no service credential is ever returned), the client Messaging API (the caller's principal and scope ids, organization colleagues, and an allowlist of end-user conversation, MLS Delivery Service, attachment and history-transfer operations relayed with the caller's principal signed) and the platform-admin chat migration controls (state machine, parity, rollback)."),
        (name = "Waiting Room", description = "Admit/Deny participants from waiting room"),
        (name = "Media Control", description = "Control audio, video, and screen sharing"),
        (name = "Chat", description = "In-meeting chat with hybrid real-time architecture.\n\n## Architecture\n- **LiveKit Data Channels**: Real-time message delivery via `publishData()` and `RoomEvent.DataReceived`\n- **REST API**: Message persistence and history loading for late joiners\n\n## Flow\n1. **Send**: POST /chat (persist) → LiveKit publishData() (broadcast)\n2. **Receive**: Listen to RoomEvent.DataReceived for instant delivery\n3. **Late Join**: GET /chat to load message history\n\n## Notes\n- Messages are **volatile** - automatically deleted when meeting ends\n- Only participants with 'joined' status can send/receive messages"),
        (name = "Users", description = "User profile management and account operations.\n\n## Endpoints\n- **GET /users/me**: Get current user profile\n- **PATCH /users/me**: Update profile (username only)\n- **POST /users/me/deactivate**: Deactivate account (soft delete; needs a step-up token for DEACTIVATE_ACCOUNT)\n\n## Notes\n- All endpoints require JWT authentication\n- Deactivated users are blocked by auth middleware\n- Deactivation immediately invalidates all tokens\n- Administrators reactivate an account with `PATCH /api/v1/admin/users/{id}` (`status: active`)"),
        (name = "Webhooks", description = "**Deprecated** (Blueprint v1.0 Section 16, PLT-003). Legacy per-user meeting webhook registrations.\n\n## Nothing is delivered\nRegistrations are stored, but no event is ever delivered to a registered endpoint: the Meeting Service has no webhook dispatcher. The event types (`meeting_start`, `meeting_end`, `participant_join`, `participant_leave`) are accepted for compatibility only.\n\n## Deprecation\nEvery response carries `Deprecation` and `Sunset` headers. Removal still requires a measured zero-consumer window (register decision D-9).\n\n## Replacement\nUse Developer Platform webhooks (`/v1/environments/{environmentId}/webhooks`): signed delivery, delivery history, replay and secret rotation. Migration guide: `docs/api/legacy-meeting-webhooks-deprecation.md`."),
        (name = "Notifications (v2)", description = "Document 4 §3.1: the signed-in user's notification inbox, unread count, notification preferences and push endpoints of the unified Notification Service, relayed like the Communications API: the Account session is verified here and the request is forwarded with the server-resolved account. Answers are the Notification Service's own (schemaVersion 1; Retry-After and ETag relayed; never codes, tokens, addresses, push handles, E2EE text or URLs). Answers 503 SERVICE_UNAVAILABLE until the relay is configured."),
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
        (name = "Organizations (v2)", description = "First-party CF Meet business organizations (not developer-platform API workspaces) and the organization membership used by ORGANIZATION_ONLY access. Membership is resolved server-side; clients can never assert it. The Organizations Service is authoritative for organization data (production since the first activation): these routes relay to it with the signed-in user as the verified acting user (create, profile, members, invitations, teams, policies, ownership transfer, lifecycle, audit, provisioning). A deployment whose Organizations Service is not authoritative answers FEATURE_DISABLED. Organization Calling and Messaging relay allowlisted paths to the Calling and Messaging services, which own their data. The public Swagger selector serves these operations as the dedicated Organizations document."),
        (name = "Apps (v2)", description = "ADR 0064 / 0065 CF Meet Apps: the OAuth consent relay for https://cfmeet.com/oauth/authorize (the signed-in account approves or denies an app's authorization request; Platform Access is the token authority), the account's connected apps, an organization's installed apps (organization OWNER / ADMIN) and in-meeting apps (apps with a meeting surface installed for the meeting owner or organization, 5-minute Ed25519 app-context tokens and their public keys). Each feature is off unless enabled (FEATURE_DISABLED)."),
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
        version = "1.9.0",
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
            // ADR 0062: a partner user's CF Meet user token (device credential). Accepted
            // only by the operations that list it (the conference join).
            components.add_security_scheme(
                "userToken",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::HttpBuilder::new()
                        .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
                        .bearer_format("cfut")
                        .description(Some(
                            "CF Meet user token of a partner user (`cfut_<sbx|test|live>_<JWS>`), minted by the partner's server for one user of its application environment and sent by the device as `Authorization: Bearer cfut_…`. Platform Access verifies it live on every use (a successful answer is cached for at most 30 seconds): the user must be ACTIVE and the minting credential, environment, application and tenant active. Only the conference join accepts it; the join returns the participant token the device uses afterwards.",
                        ))
                        .build(),
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

/// Part VI §51: the machine-callable operations
/// ([`crate::services::platform::operations::OPERATIONS`], the single source of truth)
/// additionally accept a platform credential through the API Gateway. Each gets the
/// `platformCredential` security alternative (its role is the required scope) and
/// `x-cfmeet-required-scope`, from which the gateway's route manifest is generated.
/// Blueprint v1.0 Section 16 (MTG-020): `request_body = Option<T>` marks a body optional
/// (`required: false`, what the `V2OptionalJson` extractor accepts: an absent / empty
/// body), but utoipa also wraps the schema as nullable. A JSON `null` body is refused
/// (400), so the schema stays the plain reference; only `required` says "optional".
struct OptionalBodyAddon;

impl utoipa::Modify for OptionalBodyAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::{schema::Schema, RefOr, Required};
        for item in openapi.paths.paths.values_mut() {
            for operation in item.operations.values_mut() {
                let Some(body) = operation.request_body.as_mut() else {
                    continue;
                };
                if !matches!(body.required, Some(Required::False)) {
                    continue;
                }
                for content in body.content.values_mut() {
                    if let RefOr::T(Schema::AllOf(all)) = &content.schema {
                        if all.nullable && all.items.len() == 1 {
                            content.schema = all.items[0].clone();
                        }
                    }
                }
            }
        }
    }
}

struct PlatformCredentialAddon;

impl utoipa::Modify for PlatformCredentialAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::{
            security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme},
            PathItemType,
        };
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "platformCredential",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("cfmk_<sbx|test|live>_<public id>_<secret>")
                        .description(Some(
                            "Customer platform API credential issued to an application environment's service account, sent to the CF Meet public API Gateway as `Authorization: Bearer <credential>`. The roles listed in an operation's security requirement are the platform scopes the credential must hold (meetings.read / meetings.write). The gateway forwards the request without the credential; the Meeting Service has Platform Access re-verify it and acts as the environment's machine caller, which manages only meetings owned by its own application environment (any other meeting is 404). Never accepted on internal routes.",
                        ))
                        .build(),
                ),
            );
            // ADR 0064: an app's OAuth access token for one CF Meet user, accepted by the
            // API Gateway on the same operations (`OAUTH_ACCESS_TOKEN`).
            components.add_security_scheme(
                "oauthAccessToken",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("cfoa_<sbx|test|live>_…")
                        .description(Some(
                            "OAuth access token of a CF Meet App acting for one CF Meet user (ADR 0064), sent to the public API Gateway as `Authorization: Bearer cfoa_…`. The roles listed are the OAuth scopes the grant must hold (meetings:read / meetings:write). The app acts as that user on the user's own meetings only (read: meetings the user owns, hosts or co-hosts; write: meetings the user owns; any other meeting is 404) and can never join, receive media, read recordings or administer. Bot tokens are refused.",
                        ))
                        .build(),
                ),
            );
        }
        for op in crate::services::platform::operations::OPERATIONS {
            let Some(item) = openapi.paths.paths.get_mut(op.path) else {
                continue;
            };
            let kind = match op.method {
                "GET" => PathItemType::Get,
                "POST" => PathItemType::Post,
                "PATCH" => PathItemType::Patch,
                "PUT" => PathItemType::Put,
                "DELETE" => PathItemType::Delete,
                _ => continue,
            };
            let Some(operation) = item.operations.get_mut(&kind) else {
                continue;
            };
            operation
                .security
                .get_or_insert_with(Vec::new)
                .push(SecurityRequirement::new("platformCredential", [op.scope]));
            if let Some(oauth_scope) = crate::services::apps::delegated::oauth_scope_of(op.scope) {
                operation
                    .security
                    .get_or_insert_with(Vec::new)
                    .push(SecurityRequirement::new("oauthAccessToken", [oauth_scope]));
            }
            operation
                .extensions
                .get_or_insert_with(Default::default)
                .insert("x-cfmeet-required-scope".into(), op.scope.into());
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

/// Public OpenAPI documents for peer services (Blueprint v1.0 Section 16, PLT-006).
///
/// ONE source of truth: the generated public bundles `packages/contracts/bundled/public/**`
/// (`scripts/contracts/public_api.py` + `scripts/contracts/bundle_openapi.py`, derived from
/// the source contracts and the gateway catalogue `infra/api-gateway/public-routes.json`).
/// They hold exactly the operations the gateway classifies `PUBLIC_CUSTOMER`; `EXCLUDED`
/// ones (e.g. Contact Vault `CONTACT_VAULT_PRIVATE`), internal, operator and provider
/// operations are absent, and every `$ref` is resolved inside the document. The gateway
/// (`GET /docs`) and Calling's documentation page serve the same files, so the three
/// renderers can no longer publish contradictory surfaces. Internal and operator contracts
/// are documented only in the private engineering catalogue
/// (`scripts/swagger/build-private-docs.py`), never here.
///
/// These documents are served by the documentation host only; they do NOT proxy APIs or
/// weaken the peer services' auth.
macro_rules! public_bundle {
    ($domain:literal) => {
        include_str!(concat!(
            "../../../../packages/contracts/bundled/public/",
            $domain,
            "/v1.openapi.json"
        ))
    };
}

/// (Swagger name, served path, public bundle). The Meeting Service document is generated
/// at runtime (it carries the configured server list) and is listed first. ADR 0061: the
/// Communications Hub is first-party only (`EXCLUDED FIRST_PARTY_COMMUNICATIONS_HUB`), so
/// its public bundle is empty by design and is not published on the doc host.
const PEER_DOCS: [(&str, &str, &str); 7] = [
    (
        "Calling",
        "/api-docs/calling.json",
        public_bundle!("calling"),
    ),
    (
        "Numbering / DCNs",
        "/api-docs/numbering.json",
        public_bundle!("numbering"),
    ),
    (
        "Privacy Relay",
        "/api-docs/privacy-relay.json",
        public_bundle!("privacy-relay"),
    ),
    (
        "SMS Gateway",
        "/api-docs/sms-gateway.json",
        public_bundle!("sms"),
    ),
    (
        "Messaging",
        "/api-docs/messaging.json",
        public_bundle!("messaging"),
    ),
    (
        "Developer Platform",
        "/api-docs/developer-platform.json",
        public_bundle!("developer-platform"),
    ),
    (
        "API Gateway",
        "/api-docs/api-gateway.json",
        public_bundle!("gateway"),
    ),
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
    let mut urls = vec![
        serde_json::json!({ "name": "Meetings", "url": "/api-docs/openapi.json" }),
        serde_json::json!({ "name": "Organizations", "url": ORGANIZATIONS_DOC_PATH }),
    ];
    urls.extend(
        PEER_DOCS
            .iter()
            .map(|(name, path, _)| serde_json::json!({ "name": name, "url": path })),
    );
    serde_json::json!({ "urls": urls, "primaryName": "Meetings" })
}

/// Public API documentation is an audience-specific view of a contract. Peer documents
/// are already the generated public bundles (the audience decision lives in
/// `infra/api-gateway/public-routes.json`, not here); the generic structural filter below
/// is defense in depth and, for the Meeting Service's own runtime document, removes its
/// admin and probe routes. No per-service path list is kept here: hiding an operation the
/// gateway publishes, or publishing one it excludes, is a catalogue decision.
/// Filtering happens before the JSON reaches a public endpoint (not just in the UI).
fn public_doc(service: &str, raw: &str) -> String {
    let mut doc: serde_json::Value =
        serde_json::from_str(raw).expect("checked-in OpenAPI contract must be valid JSON");
    let paths = doc["paths"]
        .as_object_mut()
        .expect("checked-in OpenAPI must have paths");
    paths.retain(|path, item| {
        if path.starts_with("/internal/")
            || path.starts_with("/provider-")
            || path.starts_with("/v1/admin/")
            || path.starts_with("/api/v1/admin/")
            || path.starts_with("/api/v2/admin/")
            || matches!(path.as_str(), "/health" | "/ready" | "/openapi.json")
        {
            return false;
        }
        let methods = item
            .as_object()
            .expect("checked-in OpenAPI path item must be an object");
        methods.keys().any(|method| {
            matches!(
                method.as_str(),
                "get" | "post" | "put" | "patch" | "delete" | "head" | "options" | "trace"
            )
        })
    });
    if service == "Messaging" {
        // Document actual native routing without exposing application/service credentials
        // or inventing a second message API. Operator and internal paths are not in the bundle.
        for (path, item) in doc["paths"].as_object_mut().unwrap() {
            for (method, operation) in item.as_object_mut().unwrap() {
                if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                    continue;
                }
                if let Some(native) =
                    crate::services::unified_messaging::relay::native_public_path(method, path)
                {
                    operation["x-cfmeet-native-path"] = serde_json::json!(native);
                    operation["x-cfmeet-native-auth"] = serde_json::json!("ACCOUNT_BEARER_SESSION");
                    operation["x-cfmeet-native-principal"] =
                        serde_json::json!("SERVER_DERIVED_DO_NOT_SEND_CF_ACTING_PRINCIPAL");
                }
            }
        }
        doc["info"]["description"] = serde_json::json!(format!("{}\nCF Meet account clients use each operation's x-cfmeet-native-path with their existing Account bearer session. Canonical /v1 routes use the separately authenticated application gateway and are not a substitute for native routing. Device realtime uses a short-lived client token and a single-use WebSocket subprotocol ticket. PRIVATE_E2EE requires approved enrollment, valid client cryptographic state and contact policy; it never falls back to MANAGED. In production, personal E2EE runs under the owner's recorded release decision: no independent cryptographic assessment has been performed (NOT_PERFORMED) and the key-transparency log is UNWITNESSED; no certification is claimed. Operator and internal service operations are excluded from this document.",doc["info"]["description"].as_str().unwrap_or("")));
    }
    // The generated public bundles carry each operation's source tag (and summary) but no
    // top-level `tags` array, so every used tag is declared here; no operation is added or
    // relabelled. (The former API Gateway context-operation label patch is gone: the
    // gateway bundle labels that operation itself — see the test
    // `public_bundles_already_label_every_operation`.)
    // A section whose operations were all filtered out (e.g. "Admin") would still render
    // as an empty Swagger heading; publish only the sections that still hold operations.
    let used: std::collections::BTreeSet<String> = doc["paths"]
        .as_object()
        .into_iter()
        .flat_map(|paths| paths.values())
        .filter_map(serde_json::Value::as_object)
        .flat_map(|item| item.values())
        .filter_map(|operation| operation["tags"].as_array())
        .flatten()
        .filter_map(|tag| tag.as_str().map(str::to_owned))
        .collect();
    if doc["tags"].as_array().is_none() {
        doc["tags"] = serde_json::json!([]);
    }
    if let Some(tags) = doc["tags"].as_array_mut() {
        tags.retain(|tag| tag["name"].as_str().is_some_and(|name| used.contains(name)));
        let declared: std::collections::BTreeSet<String> = tags
            .iter()
            .filter_map(|tag| tag["name"].as_str().map(str::to_owned))
            .collect();
        for name in used.difference(&declared) {
            tags.push(serde_json::json!({ "name": name }));
        }
    }
    serde_json::to_string(&doc).expect("filtered public OpenAPI document serializes")
}

/// Swagger section of the first-party Organizations routes in the Meeting Service contract.
const ORGANIZATIONS_TAG: &str = "Organizations (v2)";
/// Served path of the dedicated Organizations document in the public selector.
const ORGANIZATIONS_DOC_PATH: &str = "/api-docs/organizations.json";

/// Sections of the dedicated Organizations document, in display order.
const ORGANIZATIONS_SECTIONS: [(&str, &str); 11] = [
    ("Organizations", "Create a business organization (the caller becomes OWNER), read and update its profile, and list the organizations the caller verifiably belongs to."),
    ("Members", "Organization membership: list members, add or change a membership, change a member's role or status, and leave an organization."),
    ("Invitations", "Invite by email or CF Meet ID, list and revoke invitations; the invitee accepts or declines."),
    ("Teams", "Teams inside an organization and their members."),
    ("Policies", "The organization policy, changed with compare-and-swap."),
    ("Lifecycle", "Ownership transfer, suspension, reactivation and archival (OWNER)."),
    ("Audit", "An organization's audit history."),
    ("Provisioning", "An organization's technical provisioning state (service bindings for Calling and Messaging)."),
    ("Workspaces", "The caller's workspaces (Personal and each organization) and the capabilities the server derives for each."),
    ("Organization Calling", "Organization Enterprise Calling, relayed to the Calling Service for allowlisted paths. Calling owns the call data."),
    ("Organization Messaging", "Organization-scope Messaging: session-device tokens, colleague resolution and an allowlisted relay to the Messaging Service, which owns the conversation data."),
];

/// Section of an Organizations operation in the dedicated document, from its path.
fn organizations_section(path: &str) -> &'static str {
    if path.starts_with("/api/v2/me/workspaces") {
        return "Workspaces";
    }
    if path.starts_with("/api/v2/organization-invitations") {
        return "Invitations";
    }
    let rest = path
        .strip_prefix("/api/v2/organizations/{id}")
        .unwrap_or_default();
    let section = |prefix: &str| rest == prefix || rest.starts_with(&format!("{prefix}/"));
    if section("/members") {
        "Members"
    } else if section("/invitations") {
        "Invitations"
    } else if section("/teams") {
        "Teams"
    } else if section("/policies") {
        "Policies"
    } else if section("/calling") {
        "Organization Calling"
    } else if section("/messaging") {
        "Organization Messaging"
    } else if ["/suspend", "/reactivate", "/archive", "/ownership-transfer"]
        .iter()
        .any(|p| section(p))
    {
        "Lifecycle"
    } else if section("/audit-events") {
        "Audit"
    } else if section("/provisioning") {
        "Provisioning"
    } else {
        "Organizations"
    }
}

fn is_http_method(key: &str) -> bool {
    matches!(
        key,
        "get" | "post" | "put" | "patch" | "delete" | "head" | "options" | "trace"
    )
}

/// Names of the component schemas `value` references, directly or through other schemas.
fn referenced_schemas(
    value: &serde_json::Value,
    schemas: &serde_json::Map<String, serde_json::Value>,
) -> std::collections::BTreeSet<String> {
    fn collect(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(name) = map
                    .get("$ref")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|r| r.strip_prefix("#/components/schemas/"))
                {
                    out.push(name.to_owned());
                }
                map.values().for_each(|v| collect(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut pending = Vec::new();
    collect(value, &mut pending);
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = pending.pop() {
        if seen.insert(name.clone()) {
            if let Some(schema) = schemas.get(&name) {
                collect(schema, &mut pending);
            }
        }
    }
    seen
}

/// Splits the filtered public Meetings document in two: the first-party Organizations
/// operations move, unchanged except for their section, into a dedicated document, and
/// the Meetings document keeps everything else. Each operation is documented exactly
/// once; paths, request and response schemas, authentication and servers are unchanged.
fn split_organizations(meeting_doc: &str) -> (String, String) {
    let mut meeting: serde_json::Value =
        serde_json::from_str(meeting_doc).expect("filtered Meetings document is valid JSON");
    let mut organization_paths = serde_json::Map::new();
    let paths = meeting["paths"]
        .as_object_mut()
        .expect("Meetings document has paths");
    for (path, item) in paths.iter_mut() {
        let item = item.as_object_mut().expect("path item is an object");
        let moved: Vec<String> = item
            .iter()
            .filter(|(key, operation)| {
                is_http_method(key)
                    && operation["tags"]
                        .as_array()
                        .is_some_and(|tags| tags.iter().any(|t| t == ORGANIZATIONS_TAG))
            })
            .map(|(key, _)| key.clone())
            .collect();
        if moved.is_empty() {
            continue;
        }
        let mut target: serde_json::Map<String, serde_json::Value> = item
            .iter()
            .filter(|(key, _)| !is_http_method(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for method in moved {
            let mut operation = item.remove(&method).expect("operation listed above");
            operation["tags"] = serde_json::json!([organizations_section(path)]);
            target.insert(method, operation);
        }
        organization_paths.insert(path.clone(), serde_json::Value::Object(target));
    }
    paths.retain(|_, item| {
        item.as_object()
            .is_some_and(|i| i.keys().any(|k| is_http_method(k)))
    });
    if let Some(tags) = meeting["tags"].as_array_mut() {
        tags.retain(|tag| tag["name"] != ORGANIZATIONS_TAG);
    }
    meeting["info"]["description"] = serde_json::json!(format!(
        "{}\n\nOrganizations, memberships, invitations, teams, policies, workspaces and the \
         organization Calling and Messaging relays are documented in the Organizations \
         document of this selector (`{ORGANIZATIONS_DOC_PATH}`).",
        meeting["info"]["description"]
            .as_str()
            .unwrap_or("")
            .trim_end()
    ));

    let empty = serde_json::Map::new();
    let all_schemas = meeting["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty);
    let organization_paths = serde_json::Value::Object(organization_paths);
    let schemas: serde_json::Map<String, serde_json::Value> =
        referenced_schemas(&organization_paths, all_schemas)
            .into_iter()
            .filter_map(|name| all_schemas.get(&name).map(|s| (name, s.clone())))
            .collect();
    let mut components = meeting["components"].clone();
    components["schemas"] = serde_json::Value::Object(schemas);
    let mut organizations = serde_json::json!({
        "openapi": meeting["openapi"],
        "info": {
            "title": "CF Meet Organizations API",
            "version": meeting["info"]["version"],
            "description": "First-party CF Meet business organizations: organization profile, \
    membership, invitations, teams, policies, lifecycle (ownership transfer, suspension, \
    reactivation, archival), audit history, provisioning and workspaces, plus the organization-scoped \
    Calling and Messaging relays. These are not Developer Platform API workspaces.\n\n\
    The Organizations Service is authoritative for organization data. The CF Meet API serves these \
    routes at `/api/v2` and relays them to the Organizations Service with the signed-in user as the \
    verified acting user; membership is always resolved on the server and clients can never assert it. \
    Organization Calling and Messaging keep their own data in the Calling and Messaging services and \
    relay only allowlisted paths.\n\n\
    Authentication: the CF Meet account bearer session, as for every `/api/v2` route.\n\n\
    Not in this public document: platform administration (`/api/v2/admin/organizations`) and the \
    Organizations Service's internal service-to-service API (`/internal/v1`), which are documented only \
    in the private engineering catalogue.",
        },
        "servers": meeting["servers"],
        "paths": organization_paths,
        "components": components,
        "tags": ORGANIZATIONS_SECTIONS
            .iter()
            .map(|(name, description)| serde_json::json!({ "name": name, "description": description }))
            .collect::<Vec<_>>(),
    });
    if let Some(security) = meeting.get("security") {
        organizations["security"] = security.clone();
    }
    // Declare only sections that still hold operations (an empty heading would render).
    let used: std::collections::BTreeSet<String> = organizations["paths"]
        .as_object()
        .into_iter()
        .flat_map(|paths| paths.values())
        .filter_map(serde_json::Value::as_object)
        .flat_map(|item| item.values())
        .filter_map(|operation| operation["tags"].as_array())
        .flatten()
        .filter_map(|tag| tag.as_str().map(str::to_owned))
        .collect();
    if let Some(tags) = organizations["tags"].as_array_mut() {
        tags.retain(|tag| tag["name"].as_str().is_some_and(|name| used.contains(name)));
    }
    (
        serde_json::to_string(&meeting).expect("Meetings document serializes"),
        serde_json::to_string(&organizations).expect("Organizations document serializes"),
    )
}

fn contract_doc(body: String) -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, "public, max-age=300"),
        ],
        body,
    )
}

/// D1: whether Swagger UI and the served OpenAPI documents are mounted. `API_DOCS_ENABLED`
/// (`true` / `false`) decides when set; otherwise they are served only where the runtime is
/// explicitly a development or staging one (`APP_ENV` dev / development / local / test, or
/// `CF_RUNTIME_ENVIRONMENT` local / test / staging) and never in production, where
/// customer documentation is published through the developer portal (the API Gateway's
/// public subset) instead. Unknown or missing values count as production (fail closed).
pub fn docs_enabled_from(get: impl Fn(&str) -> Option<String>) -> bool {
    let value = |k: &str| {
        get(k)
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default()
    };
    match value("API_DOCS_ENABLED").as_str() {
        "true" | "1" | "on" | "yes" => return true,
        "false" | "0" | "off" | "no" => return false,
        _ => {}
    }
    if value("CF_RUNTIME_ENVIRONMENT") == "production" {
        return false;
    }
    matches!(
        value("CF_RUNTIME_ENVIRONMENT").as_str(),
        "local" | "test" | "staging"
    ) || crate::config::is_development_env(Some(&value("APP_ENV")))
}

/// [`docs_enabled_from`] over the process environment.
pub fn docs_enabled() -> bool {
    docs_enabled_from(|k| std::env::var(k).ok())
}

/// Creates the CF Meet Swagger UI with a service selector. The Meeting Service remains
/// the default and keeps its original /api-docs/openapi.json contract unchanged.
/// Peer service documents come from the bundled renderings of the checked-in contracts.
pub fn swagger_router(_server_addr: &str) -> Router {
    let mut openapi = ApiDoc::openapi();

    // Documentation is read-only: never advertise a bind address such as 0.0.0.0
    // as a callable OpenAPI server. Only the configured public origin is meaningful.
    let configured = std::env::var("SWAGGER_SERVER_URL").ok();
    let mut public_server = utoipa::openapi::Server::new(public_server_url(configured.as_deref()));
    public_server.description = Some("Meeting Service deployment".to_string());
    openapi.servers = Some(vec![public_server]);
    // Audience note (PLT-006): this is the Meeting Service's own direct-ingress contract,
    // first-party routes included; the gateway publishes the third-party subset.
    openapi.info.description = Some(format!(
        "{}\n\nThis document is the Meeting Service's own direct-ingress contract (CF Meet \
         first-party routes included). Third-party integrations use the API Gateway's public \
         Meetings document (`/openapi/meetings.json`, generated from \
         `packages/contracts/bundled/public/meetings`).",
        openapi.info.description.as_deref().unwrap_or("").trim_end()
    ));
    let (meeting_doc, organizations_doc) = split_organizations(&public_doc(
        "Meetings",
        &openapi.to_json().expect("OpenAPI document serializes"),
    ));

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
        )
        .route(
            ORGANIZATIONS_DOC_PATH,
            get(move || {
                let doc = organizations_doc.clone();
                async move { contract_doc(doc) }
            }),
        );
    for (name, path, body) in PEER_DOCS {
        let public_body = public_doc(name, body);
        router = router.route(
            path,
            get(move || {
                let doc = public_body.clone();
                async move { contract_doc(doc) }
            }),
        );
    }
    router
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Served `/api-docs/messaging.json` (the public Messaging bundle, PLT-006).
    const SERVED_MESSAGING_OPERATIONS: usize = 107;
    const SERVED_MESSAGING_NATIVE: usize = 74;

    fn peer(name: &str) -> &'static str {
        PEER_DOCS
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, _, raw)| *raw)
            .unwrap_or_else(|| panic!("no peer document {name}"))
    }

    #[test]
    fn docs_are_off_in_production_and_on_for_development_and_staging() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        // Production (explicit, or nothing said at all) never serves them.
        assert!(!docs_enabled_from(env(&[(
            "CF_RUNTIME_ENVIRONMENT",
            "production"
        )])));
        assert!(!docs_enabled_from(env(&[
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            ("APP_ENV", "dev")
        ])));
        assert!(!docs_enabled_from(env(&[])));
        assert!(!docs_enabled_from(env(&[("APP_ENV", "production")])));
        // Local and staging keep them.
        assert!(docs_enabled_from(env(&[("APP_ENV", "dev")])));
        assert!(docs_enabled_from(env(&[("APP_ENV", "test")])));
        assert!(docs_enabled_from(env(&[(
            "CF_RUNTIME_ENVIRONMENT",
            "staging"
        )])));
        assert!(docs_enabled_from(env(&[(
            "CF_RUNTIME_ENVIRONMENT",
            "local"
        )])));
        // The explicit switch wins either way.
        assert!(!docs_enabled_from(env(&[
            ("APP_ENV", "dev"),
            ("API_DOCS_ENABLED", "false")
        ])));
        assert!(docs_enabled_from(env(&[
            ("CF_RUNTIME_ENVIRONMENT", "production"),
            ("API_DOCS_ENABLED", "true")
        ])));
    }

    #[test]
    fn peer_openapi_specs_are_valid_and_expose_expected_operations() {
        let specs = [
            (peer("Calling"), "/v1/calls"),
            (peer("Numbering / DCNs"), "/v1/numbers/assignments"),
            (peer("Privacy Relay"), "/v1/privacy-relay/contexts"),
            (peer("SMS Gateway"), "/v1/sms/messages"),
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
    async fn swagger_serves_only_public_contracts_from_the_doc_host() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;

        let router = swagger_router("127.0.0.1:18081");
        for path in [
            "/api-docs/openapi.json",
            "/api-docs/organizations.json",
            "/api-docs/calling.json",
            "/api-docs/numbering.json",
            "/api-docs/privacy-relay.json",
            "/api-docs/sms-gateway.json",
            "/api-docs/messaging.json",
            "/api-docs/developer-platform.json",
            "/api-docs/api-gateway.json",
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
        for forbidden in [
            "/api-docs/platform-access.json",
            "/api-docs/organizations-internal.json",
            "/api-docs/privacy-relay-internal.json",
            "/api-docs/telephony.json",
            "/api-docs/messaging-internal.json",
            "/api-docs/sms-gateway-internal.json",
            // ADR 0061: the Communications Hub is first-party only.
            "/api-docs/communications.json",
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(forbidden)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{forbidden}");
        }
    }

    const HTTP_METHODS: [&str; 8] = [
        "get", "post", "put", "patch", "delete", "head", "options", "trace",
    ];

    fn operations(doc: &serde_json::Value) -> std::collections::BTreeSet<(String, String)> {
        doc["paths"]
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(path, item)| {
                HTTP_METHODS
                    .iter()
                    .filter(|m| item.get(**m).is_some())
                    .map(move |m| (m.to_uppercase(), path.clone()))
            })
            .collect()
    }

    /// Gateway catalogue domain of each served peer document.
    fn catalogue_domain(name: &str) -> &'static str {
        match name {
            "Calling" => "calling",
            "Numbering / DCNs" => "numbering",
            "Privacy Relay" => "privacy-relay",
            "SMS Gateway" => "sms",
            "Messaging" => "messaging",
            "Developer Platform" => "developer-platform",
            "API Gateway" => "gateway",
            other => panic!("peer document {other} has no gateway catalogue domain"),
        }
    }

    /// PLT-006: each served peer document holds EXACTLY the operations the gateway
    /// catalogue (`infra/api-gateway/route-manifest.json`, generated from
    /// `public-routes.json`) classifies `PUBLIC_CUSTOMER` for that domain — nothing hidden
    /// ad hoc, and no `EXCLUDED` / internal / operator / provider operation published.
    #[test]
    fn served_peer_documents_match_the_gateway_public_catalogue() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../infra/api-gateway/route-manifest.json"
        ))
        .expect("route manifest is valid JSON");
        let catalogue = manifest["catalogue"].as_array().expect("catalogue");
        let class_of = |domain: &str, method: &str, path: &str| -> Option<String> {
            catalogue
                .iter()
                .find(|e| {
                    e["domain"] == domain
                        && e["method"].as_str().map(str::to_uppercase).as_deref() == Some(method)
                        && e["path"] == path
                })
                .and_then(|e| e["class"].as_str().map(str::to_owned))
        };
        for (name, _, raw) in PEER_DOCS {
            let domain = catalogue_domain(name);
            let served: serde_json::Value = serde_json::from_str(&public_doc(name, raw)).unwrap();
            let served_ops = operations(&served);
            let bundle_ops = operations(&serde_json::from_str(raw).unwrap());
            assert_eq!(served_ops, bundle_ops, "{name}: served != generated bundle");
            let public_ops: std::collections::BTreeSet<(String, String)> = catalogue
                .iter()
                .filter(|e| e["domain"] == domain && e["class"] == "PUBLIC_CUSTOMER")
                .map(|e| {
                    (
                        e["method"].as_str().unwrap().to_uppercase(),
                        e["path"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert!(!public_ops.is_empty(), "{name}");
            assert_eq!(
                served_ops, public_ops,
                "{name}: served operations differ from the gateway PUBLIC_CUSTOMER catalogue"
            );
            for (method, path) in &served_ops {
                assert_eq!(
                    class_of(domain, method, path).as_deref(),
                    Some("PUBLIC_CUSTOMER"),
                    "{name}: {method} {path}"
                );
            }
        }
        // The audit's concrete cases.
        let relay: serde_json::Value =
            serde_json::from_str(&public_doc("Privacy Relay", peer("Privacy Relay"))).unwrap();
        for (_, path) in operations(&relay) {
            assert!(
                !path.starts_with("/v1/privacy-relay/contacts"),
                "Contact Vault private-contact operation {path} is EXCLUDED"
            );
        }
        assert!(relay["paths"]["/v1/privacy-relay/contact-access-grants"]["post"].is_object());
        let calling: serde_json::Value =
            serde_json::from_str(&public_doc("Calling", peer("Calling"))).unwrap();
        assert!(calling["paths"]["/v1/abuse-reports"].is_object());
        assert!(operations(&calling)
            .iter()
            .all(|(_, p)| !p.starts_with("/v1/calling/media/rtc")));
    }

    /// The removed API Gateway label patch was a no-op: the generated public bundles
    /// already carry a tag, a summary and a description on every operation.
    #[test]
    fn public_bundles_already_label_every_operation() {
        for (name, _, raw) in PEER_DOCS {
            let doc: serde_json::Value = serde_json::from_str(raw).unwrap();
            for (method, path) in operations(&doc) {
                let op = &doc["paths"][&path][method.to_lowercase()];
                assert!(
                    op["tags"].as_array().is_some_and(|t| !t.is_empty()),
                    "{name}: {method} {path} has no tag"
                );
                for field in ["summary", "description", "operationId"] {
                    assert!(
                        op[field].as_str().is_some_and(|v| !v.trim().is_empty()),
                        "{name}: {method} {path} has no {field}"
                    );
                }
            }
        }
        let gateway: serde_json::Value = serde_json::from_str(peer("API Gateway")).unwrap();
        let context = &gateway["paths"]["/v1/developer-platform/context"]["get"];
        assert!(context["tags"].as_array().is_some_and(|t| !t.is_empty()));
        assert!(context["summary"].is_string());
    }

    #[test]
    fn public_specs_exclude_internal_and_privileged_operations() {
        for (name, _, raw) in PEER_DOCS {
            let public: serde_json::Value = serde_json::from_str(&public_doc(name, raw)).unwrap();
            assert!(!public["paths"].as_object().unwrap().is_empty(), "{name}");
            for key in public["paths"].as_object().unwrap().keys() {
                assert!(!key.starts_with("/internal/"), "{name}: {key}");
                assert!(!key.starts_with("/provider-"), "{name}: {key}");
                assert!(!key.starts_with("/v1/admin/"), "{name}: {key}");
            }
            let schemes = public["components"]["securitySchemes"]
                .as_object()
                .map(|s| s.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            assert!(!schemes.iter().any(|s| s == "cfServiceAuth"), "{name}");
        }
        let public_calling: serde_json::Value =
            serde_json::from_str(&public_doc("Calling", peer("Calling"))).unwrap();
        assert!(public_calling["paths"]["/v1/calls"].is_object());
        assert!(public_calling["paths"]["/internal/v1/telephony/events"].is_null());
        let public_numbering: serde_json::Value =
            serde_json::from_str(&public_doc("Numbering / DCNs", peer("Numbering / DCNs")))
                .unwrap();
        assert!(public_numbering["paths"]["/v1/numbers/search"].is_object());
        assert!(public_numbering["paths"]["/v1/admin/controls"].is_null());
        let public_messaging: serde_json::Value =
            serde_json::from_str(&public_doc("Messaging", peer("Messaging"))).unwrap();
        assert!(public_messaging["paths"]["/v1/conversations"].is_object());
        // Compliance operations are PUBLIC_CUSTOMER at the gateway (credential scopes
        // decide who may call them); they are no longer hidden ad hoc here.
        assert!(public_messaging["paths"]["/v1/messaging/legal-holds"].is_object());
        assert!(public_messaging["paths"]["/v1/messaging/policy"].is_object());
        assert_eq!(
            public_messaging["paths"]["/v1/conversations"]["post"]["x-cfmeet-native-path"],
            "/api/v2/messaging/relay/v1/conversations"
        );
        assert_eq!(
            public_messaging["paths"]["/v1/messaging/devices"]["post"]["x-cfmeet-native-path"],
            "/api/v2/messaging/devices"
        );
        assert_eq!(
            public_messaging["paths"]["/v1/messaging/client-sessions/revoke-all"]["post"]
                ["x-cfmeet-native-path"],
            "/api/v2/messaging/client-sessions/revoke-all"
        );
        // Pinned for the production acceptance script
        // (`infra/calling/production/messaging_acceptance.py`): operation count and native
        // bindings of the served Messaging document.
        let ops: Vec<&serde_json::Value> = public_messaging["paths"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|item| item.as_object().unwrap().iter())
            .filter(|(m, _)| {
                matches!(
                    m.as_str(),
                    "get" | "post" | "put" | "patch" | "delete" | "options" | "head"
                )
            })
            .map(|(_, op)| op)
            .collect();
        let native = ops
            .iter()
            .filter(|op| op.get("x-cfmeet-native-path").is_some())
            .count();
        println!(
            "served messaging: {} operations, {native} native",
            ops.len()
        );
        assert_eq!(
            (ops.len(), native),
            (SERVED_MESSAGING_OPERATIONS, SERVED_MESSAGING_NATIVE)
        );
        let description = public_messaging["info"]["description"].as_str().unwrap();
        assert!(description.contains("PRIVATE_E2EE requires approved enrollment"));
        assert!(description.contains("it never falls back to MANAGED"));
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
            PEER_DOCS.len() + 2,
            "Meetings and Organizations plus every peer document"
        );
    }

    /// The Swagger selector offers Meetings, Organizations, the six legacy services plus
    /// Part VI Developer Platform and API Gateway. Every operation of
    /// every document it serves sits in a named, declared section (never Swagger's
    /// `default`) with a summary, a description and a unique operationId. Checked on the
    /// bytes the router serves, after public filtering, so neither the contracts nor the
    /// Meeting Service handlers can regress to undocumented or `default`-grouped operations.
    #[tokio::test]
    async fn every_served_public_operation_is_labeled_and_documented() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        async fn get_json(router: &Router, uri: &str) -> serde_json::Value {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK, "{uri}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{uri}: {e}"))
        }

        let router = swagger_router("127.0.0.1:18081");
        let config = get_json(&router, "/api-docs/swagger-config.json").await;
        let urls = config["urls"].as_array().expect("selector urls");
        let names: Vec<&str> = urls.iter().map(|u| u["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "Meetings",
                "Organizations",
                "Calling",
                "Numbering / DCNs",
                "Privacy Relay",
                "SMS Gateway",
                "Messaging",
                "Developer Platform",
                "API Gateway"
            ],
            "public Swagger selector"
        );

        let dp = get_json(&router, "/api-docs/developer-platform.json").await;
        assert!(dp["paths"]["/v1/organizations"]["get"].is_object());
        assert!(dp["paths"]["/internal/v1/operator/organizations"].is_null());
        let gateway = get_json(&router, "/api-docs/api-gateway.json").await;
        assert!(gateway["paths"]["/v1/developer-platform/context"]["get"].is_object());
        assert!(gateway["paths"]["/internal/v1/gateway/context"].is_null());
        let meeting = get_json(&router, "/api-docs/openapi.json").await;
        let servers = meeting["servers"].as_array().expect("public server list");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["url"], DEFAULT_PUBLIC_SERVER_URL);

        let mut problems = Vec::new();
        for entry in urls {
            let name = entry["name"].as_str().unwrap();
            let doc = get_json(&router, entry["url"].as_str().unwrap()).await;
            let declared: std::collections::BTreeSet<&str> = doc["tags"]
                .as_array()
                .map(|tags| tags.iter().filter_map(|t| t["name"].as_str()).collect())
                .unwrap_or_default();
            let mut used = std::collections::BTreeSet::new();
            let mut operation_ids = std::collections::BTreeSet::new();
            let mut operations = 0;
            for (path, item) in doc["paths"].as_object().expect("paths") {
                for (method, op) in item.as_object().expect("path item") {
                    if !matches!(
                        method.as_str(),
                        "get" | "post" | "put" | "patch" | "delete" | "head" | "options" | "trace"
                    ) {
                        continue;
                    }
                    operations += 1;
                    let at = format!("{name}: {} {path}", method.to_uppercase());
                    let tags: Vec<&str> = op["tags"]
                        .as_array()
                        .map(|t| t.iter().filter_map(|t| t.as_str()).collect())
                        .unwrap_or_default();
                    if tags.is_empty() {
                        problems.push(format!("{at}: no section (renders under `default`)"));
                    }
                    for tag in &tags {
                        if tag.trim().is_empty() || tag.eq_ignore_ascii_case("default") {
                            problems.push(format!("{at}: section `{tag}`"));
                        } else if !declared.contains(tag) {
                            problems.push(format!("{at}: undeclared section `{tag}`"));
                        }
                        used.insert(*tag);
                    }
                    for field in ["summary", "description", "operationId"] {
                        if !op[field].as_str().is_some_and(|v| !v.trim().is_empty()) {
                            problems.push(format!("{at}: no {field}"));
                        }
                    }
                    // A doc-comment summary wrapped onto a second `///` line is cut off
                    // mid-sentence (utoipa takes the first line only): unbalanced brackets or
                    // quotes, a trailing separator, or no final period after a connective.
                    if let Some(summary) = op["summary"].as_str() {
                        let summary = summary.trim_end();
                        let last = summary
                            .rsplit(' ')
                            .next()
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if summary.matches('(').count() != summary.matches(')').count()
                            || summary.matches('`').count() % 2 == 1
                            || summary.ends_with([',', ';', ':', '(', '-', '+'])
                            || (!summary.ends_with('.')
                                && [
                                    "a", "an", "and", "as", "at", "by", "every", "for", "from",
                                    "in", "into", "of", "on", "or", "the", "to", "with",
                                ]
                                .contains(&last.as_str()))
                        {
                            problems.push(format!("{at}: truncated summary `{summary}`"));
                        }
                    }
                    if let Some(id) = op["operationId"].as_str() {
                        if !operation_ids.insert(id.to_owned()) {
                            problems.push(format!("{at}: duplicate operationId {id}"));
                        }
                    }
                }
            }
            if operations == 0 {
                problems.push(format!("{name}: no operations"));
            }
            for tag in declared.difference(&used) {
                problems.push(format!("{name}: empty section `{tag}`"));
            }
        }
        assert!(
            problems.is_empty(),
            "{} Swagger documentation problems:\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    #[tokio::test]
    async fn organizations_have_their_own_document_and_are_documented_once() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        async fn get_json(router: &Router, uri: &str) -> serde_json::Value {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK, "{uri}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        // Every public first-party Organizations operation of the Meeting Service contract.
        let contract: serde_json::Value =
            serde_json::from_str(&ApiDoc::openapi().to_json().unwrap()).unwrap();
        let expected: std::collections::BTreeSet<(String, String)> = contract["paths"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(path, _)| !path.starts_with("/api/v2/admin/"))
            .flat_map(|(path, item)| {
                item.as_object()
                    .unwrap()
                    .iter()
                    .filter(|(_, op)| {
                        op["tags"]
                            .as_array()
                            .is_some_and(|t| t.iter().any(|t| t == ORGANIZATIONS_TAG))
                    })
                    .map(move |(m, _)| (m.to_uppercase(), path.clone()))
            })
            .collect();
        assert!(
            expected.len() >= 40,
            "organizations operations: {}",
            expected.len()
        );

        let router = swagger_router("127.0.0.1:18081");
        let organizations = get_json(&router, ORGANIZATIONS_DOC_PATH).await;
        let meetings = get_json(&router, "/api-docs/openapi.json").await;
        assert_eq!(
            operations(&organizations),
            expected,
            "moved exactly the Organizations operations"
        );
        assert!(
            operations(&meetings).is_disjoint(&expected),
            "an Organizations operation is still in the Meetings document"
        );
        assert!(
            operations(&meetings).len() > 100,
            "Meetings document kept its operations"
        );
        for path in [
            "/api/v2/organizations",
            "/api/v2/organizations/{id}/teams/{teamId}/members/{memberId}",
            "/api/v2/organizations/{id}/policies",
            "/api/v2/me/workspaces",
            "/api/v2/organizations/{id}/calling/{path}",
            "/api/v2/organizations/{id}/messaging/session-tokens",
        ] {
            assert!(organizations["paths"][path].is_object(), "{path}");
        }
        // Platform administration and the internal service API stay private.
        let paths = organizations["paths"].as_object().unwrap();
        assert!(paths
            .keys()
            .all(|p| !p.starts_with("/api/v2/admin/") && !p.starts_with("/internal/")));
        assert!(meetings["tags"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != ORGANIZATIONS_TAG));
        // Same server and authentication as the Meetings document; every reference resolves.
        assert_eq!(organizations["servers"], meetings["servers"]);
        assert_eq!(
            organizations["components"]["securitySchemes"],
            meetings["components"]["securitySchemes"]
        );
        let schemas = organizations["components"]["schemas"].as_object().unwrap();
        for name in referenced_schemas(&organizations["paths"], schemas) {
            assert!(schemas.contains_key(&name), "unresolved schema {name}");
        }
        assert!(
            schemas.len() < contract["components"]["schemas"].as_object().unwrap().len(),
            "only the schemas the Organizations operations use"
        );
        assert_eq!(
            organizations_section("/api/v2/organizations/{id}/teams/{teamId}/members"),
            "Teams"
        );
        assert_eq!(
            organizations_section("/api/v2/organization-invitations/accept"),
            "Invitations"
        );
        assert_eq!(
            organizations_section("/api/v2/organizations/{id}/archive"),
            "Lifecycle"
        );
        assert_eq!(
            organizations_section("/api/v2/organizations/{id}"),
            "Organizations"
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
