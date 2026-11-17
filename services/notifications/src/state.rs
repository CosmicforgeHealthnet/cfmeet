//! Shared application state (cheap to clone).

use std::sync::Arc;

use cf_notification_client::seal::OpeningKey;
use cf_push_transport::PushProvider;
use cf_service_auth::{RuntimeEnvironment, ServiceTokenVerifier};
use sea_orm::DatabaseConnection;
use secrecy::SecretString;

use crate::{
    authorities::Authorities,
    config::EmailConfig,
    crypto::DataKeys,
    email::EmailProvider,
    metrics::{Heartbeats, Metrics},
};

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    /// Receiver-authoritative verifier of `cfmeet.notifications`.
    pub verifier: Arc<ServiceTokenVerifier>,
    pub keys: Arc<DataKeys>,
    pub opening: Option<Arc<OpeningKey>>,
    pub email: Arc<EmailConfig>,
    pub email_provider: Option<Arc<dyn EmailProvider>>,
    pub push: Option<Arc<dyn PushProvider>>,
    pub authorities: Arc<dyn Authorities>,
    pub metrics: Arc<Metrics>,
    pub heartbeats: Arc<Heartbeats>,
    pub replica_id: String,
    pub runtime_environment: RuntimeEnvironment,
    pub workers_enabled: bool,
    pub operator_emails: Vec<String>,
    pub operator_alert_token: Option<Arc<SecretString>>,
    /// Roadmap N10: the Hub preference export (copy + catch-up). `None` without the Hub
    /// owner API; the copy job then never starts.
    pub hub_preferences: Option<Arc<dyn crate::preference_copy::HubPreferenceSource>>,
}

impl AppState {
    pub fn local_test_push_allowed(&self) -> bool {
        matches!(
            self.runtime_environment,
            RuntimeEnvironment::Local | RuntimeEnvironment::Test
        )
    }

    pub fn processor(&self) -> crate::processor::Processor {
        crate::processor::Processor {
            db: self.db.clone(),
            keys: self.keys.clone(),
            opening: self.opening.clone(),
            authorities: self.authorities.clone(),
            email: self.email.clone(),
            metrics: self.metrics.clone(),
            operator_emails: self.operator_emails.clone(),
        }
    }
}
