//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_service_auth::RuntimeEnvironment;
use sea_orm::DatabaseConnection;

use crate::{
    config::{
        service_auth::ServiceAuthRuntime, AttachmentSettings, ConversationSettings, DeviceSettings,
        WorkerSettings,
    },
    domain::SharedClock,
    identity::{cache::CachingAuthority, PrincipalDirectory, Unconfigured},
    messages::managed::ManagedKeyring,
    realtime::Realtime,
    transparency::TreeHeadSigner,
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub clock: SharedClock,
    /// Platform Access credential verification behind the bounded cache.
    pub authority: Arc<CachingAuthority>,
    pub principals: Arc<dyn PrincipalDirectory>,
    pub service_auth: ServiceAuthRuntime,
    pub communications: Option<cf_communications_policy::PolicyClient>,
    pub local_send_policy: bool,
    pub local_send_generation: Option<uuid::Uuid>,
    pub devices: DeviceSettings,
    pub conversations: ConversationSettings,
    pub workers: WorkerSettings,
    /// Signs every served key-transparency tree head. `None` (not configured): the
    /// directory / proof endpoints fail closed instead of serving an unsigned head.
    pub tree_heads: Option<Arc<TreeHeadSigner>>,
    /// Key-transparency witnesses and the quorum a served head needs (none: UNWITNESSED).
    pub witnesses: Arc<crate::transparency::witness::WitnessSet>,
    /// Local durable KT checkpoint, independent of database restore. Protected
    /// directory/enrollment operations verify it at their own boundary as well.
    pub kt_checkpoint: Option<std::path::PathBuf>,
    /// Seals MANAGED message text at rest. `None`: MANAGED sending fails closed.
    pub managed_keys: Option<Arc<ManagedKeyring>>,
    /// The realtime gateway runtime of this instance.
    pub realtime: Arc<Realtime>,
    /// Protected attachment storage (unconfigured: attachments fail closed).
    pub storage: Arc<dyn cf_object_storage::ObjectStore>,
    /// Self-hosted scanner of MANAGED attachments (unconfigured: scans stay pending).
    pub scanner: Arc<dyn crate::attachments::scanner::AttachmentScanner>,
    pub attachments: AttachmentSettings,
    /// Part V H: the SMS Gateway's bridge API (unconfigured: bridges are refused).
    pub sms: Arc<dyn crate::sms_bridge::SmsTransport>,
    /// N9 push-registration facade towards the Notification Service (`None`: off, the
    /// default — Messaging behaves exactly as before).
    pub push_mirror: Option<Arc<crate::push_mirror::PushMirror>>,
    /// ADR 0062 / 0063 partner devices and push (every flag off by default: nothing
    /// partner-specific is reachable).
    pub partner: Arc<crate::partner::Partner>,
    /// ADR 0064 CF Meet Apps (`MESSAGING_APPS_ENABLED`, default off: OAuth credentials
    /// are refused and nothing app-specific happens).
    pub apps: crate::apps::AppsSettings,
    /// Gap audit S1 (`MESSAGING_ORGANIZATION_WAKEUPS=on`, default off): message wake-ups of
    /// conversations in an organization's own first-party scope go to the Notification
    /// Service (route `NEW`), addressed to the members' CF Meet accounts.
    pub organization_wakeups: bool,
    /// "Added to a group" notices (`messaging_member_notifications`) are written for the
    /// Notification Service: set when `CF_NOTIFICATIONS_BASE_URL` is configured (default
    /// off: nothing is written).
    pub member_notices: bool,
    /// ADR 0068 A6: CF Meet user plan limits (`MESSAGING_PLAN_ENFORCEMENT=on`; `None`, the
    /// default: today's limits only).
    pub plans: Option<Arc<crate::plans::PlanEnforcement>>,
}

impl AppState {
    /// Part V H: connects the SMS Bridge to the SMS Gateway.
    pub fn with_sms_transport(mut self, sms: Arc<dyn crate::sms_bridge::SmsTransport>) -> Self {
        self.sms = sms;
        self
    }

    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        authority: Arc<CachingAuthority>,
        service_auth: ServiceAuthRuntime,
    ) -> Self {
        Self {
            db,
            clock,
            authority,
            principals: Arc::new(Unconfigured),
            service_auth,
            communications: None,
            local_send_policy: false,
            local_send_generation: None,
            devices: DeviceSettings::default(),
            conversations: ConversationSettings::default(),
            workers: WorkerSettings::default(),
            tree_heads: None,
            witnesses: Arc::new(Default::default()),
            kt_checkpoint: None,
            managed_keys: None,
            realtime: Arc::new(Realtime::disabled()),
            storage: Arc::new(cf_object_storage::UnconfiguredStore),
            scanner: Arc::new(crate::attachments::scanner::UnconfiguredScanner),
            attachments: AttachmentSettings::default(),
            sms: Arc::new(crate::sms_bridge::UnconfiguredSmsTransport),
            push_mirror: None,
            partner: Arc::new(crate::partner::Partner::disabled()),
            apps: crate::apps::AppsSettings::default(),
            organization_wakeups: false,
            member_notices: false,
            plans: None,
        }
    }

    pub fn with_principals(mut self, principals: Arc<dyn PrincipalDirectory>) -> Self {
        self.principals = principals;
        self
    }

    pub fn with_devices(mut self, devices: DeviceSettings) -> Self {
        self.devices = devices;
        self
    }

    pub fn with_conversations(mut self, conversations: ConversationSettings) -> Self {
        self.conversations = conversations;
        self
    }

    pub fn with_workers(mut self, workers: WorkerSettings) -> Self {
        self.workers = workers;
        self
    }

    pub fn with_tree_head_signer(mut self, signer: Arc<TreeHeadSigner>) -> Self {
        self.tree_heads = Some(signer);
        self
    }

    pub fn with_witnesses(mut self, set: Arc<crate::transparency::witness::WitnessSet>) -> Self {
        self.witnesses = set;
        self
    }

    pub fn with_managed_keys(mut self, keys: Arc<ManagedKeyring>) -> Self {
        self.managed_keys = Some(keys);
        self
    }

    pub fn with_realtime(mut self, realtime: Arc<Realtime>) -> Self {
        self.realtime = realtime;
        self
    }

    /// Wakes this instance's outbox publisher after a commit (realtime latency).
    pub fn wake_outbox(&self) {
        self.realtime.outbox_wakeup.notify_one();
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.service_auth.signer().runtime_environment()
    }
}
