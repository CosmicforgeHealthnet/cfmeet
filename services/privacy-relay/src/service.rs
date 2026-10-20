//! The Privacy Relay domain service: the single authority for Communication Contexts,
//! Privacy Relay Sessions, communication authorization decisions and active-call
//! enforcement jobs.
//!
//! Command shape (every mutation): one PostgreSQL transaction holding the context row
//! lock writes the authoritative state, the immutable audit event, its outbox envelope,
//! the enforcement job when one is required, and the idempotency record — all or
//! nothing. Required active-call enforcement is then attempted synchronously AFTER the
//! commit; if Calling cannot confirm it, the job stays durable for retry and the response
//! says PENDING (never a false APPLIED).

use std::sync::Arc;

use cf_service_auth::RuntimeEnvironment;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    clock::SharedClock,
    config::FeatureFlag,
    db,
    enforcement::CallingEnforcer,
    error::{ErrorCode, RelayError, RelayResult},
    vocabulary::EnforcementState,
};

#[derive(Clone)]
pub struct RelayService {
    pub(crate) db: DatabaseConnection,
    pub(crate) clock: SharedClock,
    pub(crate) feature: FeatureFlag,
    pub(crate) runtime: RuntimeEnvironment,
    pub(crate) enforcer: Arc<dyn CallingEnforcer>,
    /// Part V H §103: context authority pushes to Messaging.
    pub(crate) messaging: Arc<dyn crate::messaging_authority::MessagingAuthorityPusher>,
    pub(crate) worker_id: String,
    /// CONTACT_VAULT_ENABLED (shared with the Contact Vault): public-network relay
    /// authority over protected contacts requires it.
    pub(crate) contact_feature: FeatureFlag,
    /// RELAY_NUMBER_POOL_ENABLED + cooldown (§34).
    pub(crate) relay_numbers: crate::relay_numbers::RelayNumberPoolSettings,
    /// Durable event publication transport (Part G §67).
    pub(crate) events: Arc<dyn crate::events::EventPublisher>,
    /// `RELAY_NUMBER_AUTHORITY` (Part IV Part P): LOCAL (default / rollback) or NUMBERING.
    pub(crate) authority_mode: crate::relay_numbers::authority::AuthorityMode,
    /// Numbering's relay-number authority (leases); None = not configured.
    pub(crate) number_authority: Option<Arc<dyn crate::relay_numbers::authority::NumberAuthority>>,
    /// NUMBERING mode lends numbers only once the backfill and the compatibility check
    /// passed (every ACTIVE mapping leased to exactly its assignment and generation).
    pub(crate) numbering_cutover: Arc<std::sync::atomic::AtomicBool>,
}

/// Result of a mutation: HTTP status, body and whether it replays an earlier execution.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub status: u16,
    pub body: Value,
    pub replayed: bool,
}

impl RelayService {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        feature: FeatureFlag,
        runtime: RuntimeEnvironment,
        enforcer: Arc<dyn CallingEnforcer>,
    ) -> Self {
        Self {
            db,
            clock,
            feature,
            runtime,
            enforcer,
            messaging: Arc::new(crate::messaging_authority::UnconfiguredMessagingAuthority),
            worker_id: format!("privacy-relay-{}", Uuid::new_v4().simple()),
            contact_feature: FeatureFlag::new(false),
            relay_numbers: Default::default(),
            events: Arc::new(crate::events::NoEventPublisher),
            authority_mode: Default::default(),
            number_authority: None,
            numbering_cutover: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Delivers context authority to Messaging (otherwise jobs stay PENDING).
    pub fn with_messaging_authority(
        mut self,
        pusher: Arc<dyn crate::messaging_authority::MessagingAuthorityPusher>,
    ) -> Self {
        self.messaging = pusher;
        self
    }

    /// Installs the relay-number authority (Part IV Part P).
    pub fn with_number_authority(
        mut self,
        mode: crate::relay_numbers::authority::AuthorityMode,
        authority: Option<Arc<dyn crate::relay_numbers::authority::NumberAuthority>>,
    ) -> Self {
        self.authority_mode = mode;
        self.number_authority = authority;
        self
    }

    /// The relay-number authority mode.
    pub fn authority_mode(&self) -> crate::relay_numbers::authority::AuthorityMode {
        self.authority_mode
    }

    /// Whether NUMBERING mode completed its verified cutover.
    pub fn numbering_cutover_verified(&self) -> bool {
        self.numbering_cutover
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The transport committed outbox events are published through (Part G §67).
    pub fn with_event_publisher(mut self, events: Arc<dyn crate::events::EventPublisher>) -> Self {
        self.events = events;
        self
    }

    /// Shares the Contact Vault kill switch with this service.
    pub fn with_contact_feature(mut self, flag: FeatureFlag) -> Self {
        self.contact_feature = flag;
        self
    }

    /// The relay-number pool kill switch and cooldown.
    pub fn with_relay_number_pool(
        mut self,
        flag: FeatureFlag,
        cooldown: std::time::Duration,
    ) -> Self {
        self.relay_numbers = crate::relay_numbers::RelayNumberPoolSettings {
            enabled: flag,
            cooldown: chrono::Duration::from_std(cooldown).unwrap_or(chrono::Duration::hours(72)),
        };
        self
    }

    pub fn contact_vault_enabled(&self) -> bool {
        self.contact_feature.enabled()
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn feature(&self) -> &FeatureFlag {
        &self.feature
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime
    }

    pub(crate) fn require_enabled(&self) -> RelayResult<()> {
        if self.feature.enabled() {
            Ok(())
        } else {
            Err(RelayError::Refused(ErrorCode::PrivacyRelayDisabled))
        }
    }

    /// A replayed response with the CURRENT state of its enforcement job (the recorded
    /// response captured PENDING at commit time).
    pub(crate) async fn replay(&self, status: u16, mut body: Value) -> RelayResult<CommandOutcome> {
        self.refresh_enforcement(&mut body).await?;
        Ok(CommandOutcome {
            status,
            body,
            replayed: true,
        })
    }

    /// Sets `body.enforcement.status` from the jobs named by
    /// `body.enforcement.enforcementEventIds` (APPLIED only when every job is applied).
    pub(crate) async fn refresh_enforcement(&self, body: &mut Value) -> RelayResult<()> {
        let ids: Vec<Uuid> = body
            .pointer("/enforcement/enforcementEventIds")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(());
        }
        let jobs: Vec<JobStatus> = db::all(
            &self.db,
            "SELECT status FROM privacy_relay_enforcement_jobs WHERE enforcement_event_id = ANY($1)",
            vec![ids.clone().into()],
        )
        .await?;
        let applied = jobs.len() == ids.len() && jobs.iter().all(|j| j.status == "APPLIED");
        body["enforcement"]["status"] = json!(if applied {
            EnforcementState::Applied.as_str()
        } else {
            EnforcementState::Pending.as_str()
        });
        Ok(())
    }

    /// Synchronous-first delivery of the jobs a committed command created; returns the
    /// combined state (APPLIED only if Calling confirmed every one).
    pub(crate) async fn deliver_all(&self, event_ids: &[Uuid]) -> EnforcementState {
        let mut all_applied = true;
        for id in event_ids {
            match self.deliver_enforcement(*id).await {
                Ok(EnforcementState::Applied) => {}
                Ok(_) => all_applied = false,
                Err(error) => {
                    tracing::warn!(%error, enforcement_event_id = %id, "enforcement delivery deferred to retry");
                    all_applied = false;
                }
            }
        }
        if event_ids.is_empty() {
            EnforcementState::NotRequired
        } else if all_applied {
            EnforcementState::Applied
        } else {
            EnforcementState::Pending
        }
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct JobStatus {
    status: String,
}

/// The `enforcement` member of a mutation response.
pub fn enforcement_body(event_ids: &[Uuid], state: EnforcementState) -> Value {
    let state = if event_ids.is_empty() {
        EnforcementState::NotRequired
    } else {
        state
    };
    json!({
        "status": state.as_str(),
        "enforcementEventIds": event_ids,
    })
}
