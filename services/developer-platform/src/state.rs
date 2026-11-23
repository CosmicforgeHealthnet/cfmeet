//! Shared application state (cheap to clone).

use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::{
    apps::{
        port::{
            AccountProfiles, PlatformOAuthPort, UnconfiguredAccountProfiles,
            UnconfiguredPlatformOAuth,
        },
        AppSettings,
    },
    bridge_events::BridgeEvents,
    config::{service_auth::ServiceAuthRuntime, BuildFeatures, FeatureFlags},
    control::owner_delivery::OwnerDelivery,
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
    /// ADR 0064: Platform Access as the OAuth token authority (unconfigured: fail closed).
    pub oauth: Arc<dyn PlatformOAuthPort>,
    /// ADR 0064: the Meeting Service account profiles behind `/v1/oauth/userinfo`.
    pub account_profiles: Arc<dyn AccountProfiles>,
    /// ADR 0064 / 0065: reviewers and the advertised OAuth endpoints.
    pub apps: Arc<AppSettings>,
    /// ADR 0068 A4: owner-delivered secrets (the Notification Service and its sealing key);
    /// `None`: a bridge command that must deliver a secret answers 503
    /// `CAPABILITY_UNAVAILABLE`, nothing done.
    pub owner_delivery: Option<OwnerDelivery>,
    /// ADR 0068 A7: owner events to the admin bridge (on only with
    /// `CF_ADMIN_BRIDGE_BASE_URL`; off: nothing is stored).
    pub bridge_events: BridgeEvents,
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
            oauth: Arc::new(UnconfiguredPlatformOAuth),
            account_profiles: Arc::new(UnconfiguredAccountProfiles),
            apps: Arc::new(AppSettings::default()),
            owner_delivery: None,
            bridge_events: BridgeEvents::OFF,
        }
    }

    /// ADR 0068 A4: owner-delivered secrets (only with `CF_NOTIFICATIONS_BASE_URL` and
    /// `CF_NOTIFICATIONS_SEALING_KEY`).
    pub fn with_bridge_events(mut self, events: BridgeEvents) -> Self {
        self.bridge_events = events;
        self
    }

    pub fn with_owner_delivery(mut self, delivery: OwnerDelivery) -> Self {
        self.owner_delivery = Some(delivery);
        self
    }

    pub fn with_oauth(mut self, oauth: Arc<dyn PlatformOAuthPort>) -> Self {
        self.oauth = oauth;
        self
    }

    pub fn with_account_profiles(mut self, profiles: Arc<dyn AccountProfiles>) -> Self {
        self.account_profiles = profiles;
        self
    }

    pub fn with_app_settings(mut self, settings: AppSettings) -> Self {
        self.apps = Arc::new(settings);
        self
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
