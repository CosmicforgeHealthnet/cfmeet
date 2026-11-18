//! Shared application state (cheap to clone).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    config::{service_auth::ServiceAuthRuntime, BuildFeatures, FeatureFlags},
    notification_publications::Publishing,
    platform_access::{PlatformAccessPort, UnconfiguredPlatformAccess},
    push_providers::PushProviders,
    session::{HumanSessions, UnconfiguredSessions},
    webhooks::{
        migration::port::{CallingMigrationPort, UnconfiguredCalling},
        WebhookService,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub service_auth: ServiceAuthRuntime,
    pub platform_access: Arc<dyn PlatformAccessPort>,
    pub sessions: Arc<dyn HumanSessions>,
    pub flags: FeatureFlags,
    /// Named production-access reviewers (empty: any administering operator).
    pub production_reviewers: Arc<Vec<String>>,
    /// Central customer webhooks (Section 53); `None`: every webhook route is 404.
    pub webhooks: Option<WebhookService>,
    /// Calling's controlled webhook export (Section 53 migration); unconfigured: every
    /// migration command fails closed.
    pub calling_export: Arc<dyn CallingMigrationPort>,
    /// Notification Service N7 / N11: what this process enqueues for the Notification
    /// Service (default: nothing, today's behaviour).
    pub notifications: Publishing,
    /// ADR 0062 / 0063 Developer Console features (default: all off).
    pub build: BuildFeatures,
    /// ADR 0063 N-P1: the Notification Service's push provider vault and sealing key;
    /// `None`: push credential uploads are 503 `CAPABILITY_UNAVAILABLE`.
    pub push: Option<PushProviders>,
}

impl AppState {
    pub fn new(db: DatabaseConnection, service_auth: ServiceAuthRuntime) -> Self {
        Self {
            db,
            service_auth,
            platform_access: Arc::new(UnconfiguredPlatformAccess),
            sessions: Arc::new(UnconfiguredSessions),
            flags: FeatureFlags::default(),
            production_reviewers: Arc::new(Vec::new()),
            webhooks: None,
            calling_export: Arc::new(UnconfiguredCalling),
            notifications: Publishing::default(),
            build: BuildFeatures::default(),
            push: None,
        }
    }

    pub fn with_build(mut self, build: BuildFeatures) -> Self {
        self.build = build;
        self
    }

    pub fn with_push(mut self, push: PushProviders) -> Self {
        self.push = Some(push);
        self
    }

    /// Publication to the Notification Service (only with `CF_NOTIFICATIONS_BASE_URL`).
    pub fn with_notifications(mut self, notifications: Publishing) -> Self {
        self.notifications = notifications;
        self
    }

    pub fn with_platform_access(mut self, platform_access: Arc<dyn PlatformAccessPort>) -> Self {
        self.platform_access = platform_access;
        self
    }

    pub fn with_sessions(mut self, sessions: Arc<dyn HumanSessions>) -> Self {
        self.sessions = sessions;
        self
    }

    pub fn with_production_reviewers(mut self, reviewers: Vec<String>) -> Self {
        self.production_reviewers = Arc::new(reviewers);
        self
    }

    pub fn with_flags(mut self, flags: FeatureFlags) -> Self {
        self.flags = flags;
        self
    }

    pub fn with_calling_export(mut self, calling: Arc<dyn CallingMigrationPort>) -> Self {
        self.calling_export = calling;
        self
    }

    pub fn with_webhooks(mut self, webhooks: WebhookService) -> Self {
        self.webhooks = Some(webhooks);
        self
    }
}
