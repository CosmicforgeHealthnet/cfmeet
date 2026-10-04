//! Wiring for the Phase 2 meeting domain services (held in `AppState`).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{FeatureFlags, LiveKitConfig, MeetingV2Config},
    services::{
        actor::SharedOrganizationDirectory,
        admission::AdmissionService,
        appointment_authorization::{
            AppointmentAuthorizationAdapter, HttpAppointmentAuthorizationAdapter,
            UnconfiguredAppointmentAuthorization,
        },
        compat::CompatService,
        conference::ConferenceService,
        invitations::InvitationService,
        meeting_access::MeetingAccessService,
        meeting_core::{AccessAdapterRegistry, AttemptLimiter, Core, DomainCore},
        meeting_space::MeetingSpaceService,
        membership::MembershipService,
        organizations::{DbOrganizationDirectory, OrganizationService},
        recurrence::RecurrenceService,
        rtc_control::RtcControl,
        rtc_provider::{LiveKitProvider, RtcProvider},
        rtc_webhooks::RtcWebhookService,
        RedisService,
    },
};

/// Injectable dependencies (deployments and tests replace the defaults).
pub struct MeetingDomainDeps {
    pub flags: FeatureFlags,
    pub config: MeetingV2Config,
    pub organizations: SharedOrganizationDirectory,
    pub rtc: Arc<dyn RtcProvider>,
    pub adapters: AccessAdapterRegistry,
    /// HEALTHCARE_APPOINTMENT authority (denies everything until configured).
    pub appointments: Arc<dyn AppointmentAuthorizationAdapter>,
}

impl MeetingDomainDeps {
    pub fn from_env(db: &DatabaseConnection, livekit: &LiveKitConfig) -> Self {
        Self {
            flags: FeatureFlags::from_env(),
            config: MeetingV2Config::from_env(),
            organizations: Arc::new(DbOrganizationDirectory::new(db.clone())),
            rtc: Arc::new(LiveKitProvider::new(livekit)),
            adapters: AccessAdapterRegistry::default(),
            appointments: match HttpAppointmentAuthorizationAdapter::from_env() {
                Some(a) => Arc::new(a),
                None => Arc::new(UnconfiguredAppointmentAuthorization),
            },
        }
    }
}

pub struct MeetingDomain {
    pub core: Core,
    pub spaces: MeetingSpaceService,
    pub conferences: ConferenceService,
    pub recurrence: RecurrenceService,
    pub access: MeetingAccessService,
    pub admission: AdmissionService,
    pub members: MembershipService,
    pub invitations: InvitationService,
    pub compat: CompatService,
    pub organizations: OrganizationService,
    /// Part E RTC control plane (provider lifecycle, commands, mute).
    pub rtc: RtcControl,
    /// Part E provider webhook processing.
    pub rtc_webhooks: RtcWebhookService,
    // Part F — media controls & collaboration (internal services).
    pub media: crate::services::collaboration::media::MediaControlService,
    pub messaging: crate::services::collaboration::messaging::MeetingMessagingService,
    pub engagement: crate::services::collaboration::engagement::EngagementService,
    pub breakouts: crate::services::collaboration::breakouts::BreakoutService,
    pub polls: crate::services::collaboration::polls::PollService,
    pub questions: crate::services::collaboration::questions::QuestionService,
    pub prejoin: crate::services::collaboration::prejoin::PreJoinService,
}

impl MeetingDomain {
    pub fn new(
        db: DatabaseConnection,
        redis: Option<Arc<RedisService>>,
        deps: MeetingDomainDeps,
    ) -> Self {
        let passcode_limiter = AttemptLimiter::new(
            redis.clone(),
            deps.config.passcode_max_attempts,
            deps.config.passcode_window_secs,
        );
        let core: Core = Arc::new(DomainCore {
            db,
            flags: deps.flags,
            config: deps.config,
            organizations: deps.organizations,
            rtc: deps.rtc,
            adapters: deps.adapters,
            appointments: deps.appointments,
            passcode_limiter,
            collab_limiter: crate::services::collaboration::CollabLimiter::new(redis.clone()),
            redis,
        });
        Self {
            spaces: MeetingSpaceService::new(core.clone()),
            conferences: ConferenceService::new(core.clone()),
            recurrence: RecurrenceService::new(core.clone()),
            access: MeetingAccessService::new(core.clone()),
            admission: AdmissionService::new(core.clone()),
            members: MembershipService::new(core.clone()),
            invitations: InvitationService::new(core.clone()),
            compat: CompatService::new(core.clone()),
            organizations: OrganizationService::new(core.clone()),
            rtc: RtcControl::new(core.clone()),
            rtc_webhooks: RtcWebhookService::new(core.clone()),
            media: crate::services::collaboration::media::MediaControlService::new(core.clone()),
            messaging: crate::services::collaboration::messaging::MeetingMessagingService::new(
                core.clone(),
            ),
            engagement: crate::services::collaboration::engagement::EngagementService::new(
                core.clone(),
            ),
            breakouts: crate::services::collaboration::breakouts::BreakoutService::new(
                core.clone(),
            ),
            polls: crate::services::collaboration::polls::PollService::new(core.clone()),
            questions: crate::services::collaboration::questions::QuestionService::new(
                core.clone(),
            ),
            prejoin: crate::services::collaboration::prejoin::PreJoinService::new(core.clone()),
            core,
        }
    }
}
