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
    /// Part G provider backends.
    pub artifacts: crate::services::artifacts::ArtifactBackends,
    /// Part I telemetry sink.
    pub telemetry: Arc<dyn crate::services::quality::sink::QualityTelemetrySink>,
    /// Part V H: signed gateway to Platform Access / Messaging (`None` = integration off).
    pub unified_messaging: Option<crate::services::unified_messaging::SharedGateway>,
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
            artifacts: artifact_backends_from_env(livekit),
            telemetry: Arc::new(crate::services::quality::sink::PrometheusSink::new()),
            unified_messaging: unified_messaging_from_env(),
        }
    }
}

/// Fail closed: an enabled but misconfigured integration stops startup instead of
/// running half-integrated (e.g. cut over without a reachable authority).
fn unified_messaging_from_env() -> Option<crate::services::unified_messaging::SharedGateway> {
    use crate::services::unified_messaging::{
        config::UnifiedMessagingConfig, gateway::ServiceAuthGateway,
    };
    match UnifiedMessagingConfig::from_env() {
        Ok(None) => None,
        Ok(Some(config)) => match ServiceAuthGateway::new(&config) {
            Ok(g) => {
                tracing::info!(
                    environment = %config.scope.environment_id,
                    "Unified Messaging integration enabled"
                );
                Some(Arc::new(g))
            }
            Err(e) => panic!("FEATURE_UNIFIED_MESSAGING is on but misconfigured: {e}"),
        },
        Err(e) => panic!("FEATURE_UNIFIED_MESSAGING is on but misconfigured: {e}"),
    }
}

/// Production backends: LiveKit Egress composite recording + S3-compatible storage
/// when configured; transcription / translation / intelligence stay unconfigured
/// until a provider adapter is registered (no vendor is called by default).
fn artifact_backends_from_env(
    livekit: &LiveKitConfig,
) -> crate::services::artifacts::ArtifactBackends {
    use crate::services::artifacts::{recorder::LiveKitEgressRecorder, storage::S3ArtifactStorage};
    let config = crate::config::operations::ArtifactConfig::from_env();
    let mut backends = crate::services::artifacts::ArtifactBackends::default();
    if config.storage.is_configured() {
        match S3ArtifactStorage::new(config.storage.clone()) {
            Ok(s) => backends.storage = Arc::new(s),
            Err(e) => tracing::error!("artifact storage misconfigured: {e}"),
        }
    }
    if !livekit.api_key.is_empty() {
        backends.recorder = Arc::new(LiveKitEgressRecorder::new(
            livekit,
            config.recording_encoding,
        ));
    }
    backends
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
    // Part G — recording, artefacts, consent, transcription, intelligence.
    pub recordings: crate::services::artifacts::recording::RecordingService,
    pub artifacts: crate::services::artifacts::service::ArtifactService,
    pub consents: crate::services::artifacts::consent::ConsentService,
    pub transcription: crate::services::artifacts::transcription::TranscriptionService,
    pub intelligence: crate::services::artifacts::intelligence::IntelligenceService,
    // Part H — profiles, registration, backstage.
    pub profile_policies: crate::services::specialized::ProfilePolicyService,
    pub registrations: crate::services::specialized::RegistrationService,
    pub backstage: crate::services::specialized::BackstageService,
    // Part I — quality telemetry and operations.
    pub quality: crate::services::quality::service::QualityService,
    pub operations: crate::services::quality::operations::OperationsService,
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
        let unified = deps.unified_messaging.map(|g| {
            Arc::new(crate::services::unified_messaging::UnifiedMessaging::new(
                db.clone(),
                g,
            ))
        });
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
            artifacts: deps.artifacts,
            telemetry: deps.telemetry,
            unified,
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
            )
            .with_unified(core.unified.clone()),
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
            recordings: crate::services::artifacts::recording::RecordingService::new(core.clone()),
            artifacts: crate::services::artifacts::service::ArtifactService::new(core.clone()),
            consents: crate::services::artifacts::consent::ConsentService::new(core.clone()),
            transcription: crate::services::artifacts::transcription::TranscriptionService::new(
                core.clone(),
            ),
            intelligence: crate::services::artifacts::intelligence::IntelligenceService::new(
                core.clone(),
            ),
            profile_policies: crate::services::specialized::ProfilePolicyService::new(core.clone()),
            registrations: crate::services::specialized::RegistrationService::new(core.clone()),
            backstage: crate::services::specialized::BackstageService::new(core.clone()),
            quality: crate::services::quality::service::QualityService::new(core.clone()),
            operations: crate::services::quality::operations::OperationsService::new(core.clone()),
            core,
        }
    }
}
