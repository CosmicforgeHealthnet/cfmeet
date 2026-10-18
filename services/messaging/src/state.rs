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
    pub devices: DeviceSettings,
    pub conversations: ConversationSettings,
    pub workers: WorkerSettings,
    /// Signs every served key-transparency tree head. `None` (not configured): the
    /// directory / proof endpoints fail closed instead of serving an unsigned head.
    pub tree_heads: Option<Arc<TreeHeadSigner>>,
    /// Seals MANAGED message text at rest. `None`: MANAGED sending fails closed.
    pub managed_keys: Option<Arc<ManagedKeyring>>,
    /// The realtime gateway runtime of this instance.
    pub realtime: Arc<Realtime>,
    /// Protected attachment storage (unconfigured: attachments fail closed).
    pub storage: Arc<dyn cf_object_storage::ObjectStore>,
    /// Self-hosted scanner of MANAGED attachments (unconfigured: scans stay pending).
    pub scanner: Arc<dyn crate::attachments::scanner::AttachmentScanner>,
    pub attachments: AttachmentSettings,
}

impl AppState {
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
            devices: DeviceSettings::default(),
            conversations: ConversationSettings::default(),
            workers: WorkerSettings::default(),
            tree_heads: None,
            managed_keys: None,
            realtime: Arc::new(Realtime::disabled()),
            storage: Arc::new(cf_object_storage::UnconfiguredStore),
            scanner: Arc::new(crate::attachments::scanner::UnconfiguredScanner),
            attachments: AttachmentSettings::default(),
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
