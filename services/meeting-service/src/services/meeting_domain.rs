//! Wiring for the Phase 2 meeting domain services (held in `AppState`).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{FeatureFlags, LiveKitConfig, MeetingV2Config},
    services::{
        actor::{NoOrganizationDirectory, SharedOrganizationDirectory},
        admission::AdmissionService,
        compat::CompatService,
        conference::ConferenceService,
        invitations::InvitationService,
        meeting_access::MeetingAccessService,
        meeting_core::{AccessAdapterRegistry, AttemptLimiter, Core, DomainCore},
        meeting_space::MeetingSpaceService,
        membership::MembershipService,
        recurrence::RecurrenceService,
        rtc_provider::{LiveKitProvider, RtcProvider},
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
}

impl MeetingDomainDeps {
    pub fn from_env(livekit: &LiveKitConfig) -> Self {
        Self {
            flags: FeatureFlags::from_env(),
            config: MeetingV2Config::from_env(),
            organizations: Arc::new(NoOrganizationDirectory),
            rtc: Arc::new(LiveKitProvider::new(livekit)),
            adapters: AccessAdapterRegistry::default(),
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
}

impl MeetingDomain {
    pub fn new(
        db: DatabaseConnection,
        redis: Option<Arc<RedisService>>,
        deps: MeetingDomainDeps,
    ) -> Self {
        let passcode_limiter = AttemptLimiter::new(
            redis,
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
            passcode_limiter,
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
            core,
        }
    }
}
