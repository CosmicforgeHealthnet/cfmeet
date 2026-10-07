//! Application RTC calling (Part C §20–§25): the orchestration that turns the canonical
//! Call aggregate into a working app-to-app voice / video / group call.
//!
//! ```text
//! POST /v1/calls ─▶ REQUESTED ─▶ AUTHORIZED (APP_DIRECT_V1) ─▶ RESOLVING (endpoints)
//!   ─▶ ROUTING (APPLICATION route, RTC leg, attempt 1, media binding) ─▶ provider ensure
//!   ─▶ DIALING ─▶ RINGING (one invitation per endpoint) ─▶ single-winner accept
//!   ─▶ ANSWERED ─▶ credentials ─▶ verified joins ─▶ CONNECTED ─▶ END ─▶ ENDING ─▶ cleanup
//!   ─▶ ENDED
//! ```
//!
//! Every step is a [`CallService`] transaction (state, evidence, events, outbox commit
//! together); provider calls happen between transactions, never while a Call row is
//! locked; realtime signals are published only after the transaction that produced them
//! has committed. Business code here depends on the provider-neutral
//! [`CallMediaProvider`] only.

mod control;
mod create;
pub mod http;
pub mod media;
mod ringing;
mod transfer;
mod workers;

use std::sync::Arc;

use chrono::Duration;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement,
};
use serde_json::{Map, Value};
use uuid::Uuid;

pub use control::{ActionAvailability, AvailableActions};
pub use create::{CreateAppCall, CreatedAppCall};
pub use ringing::{InvitationActor, RecoveredInvitation};
pub use transfer::TransferRequest;
pub use workers::WorkerReport;

use super::{
    command::{CallCommandContext, CallScope},
    errors::{CallError, CallResult},
    kinds::ParticipantRole,
    read_model::CallSummaryView,
    service::CallService,
};
use crate::{
    config::ConfigError,
    domain::{ApplicationId, EnvironmentId, ServiceAccountId, TenantId},
    endpoints::EndpointService,
    media::{CallMediaProvider, MediaCredentialPolicy},
    models::{call_endpoints, call_invitations, call_participants, call_sessions, calls},
    platform::principals::PrincipalResolver,
    signaling::{
        bus::{RealtimeSignalBus, SignalEnvelope},
        push::{NoPushTransport, PushDispatcher},
        signals::{self, SignalType},
    },
};

/// Tunables of application calling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppCallingConfig {
    /// Server-time ring window of an invitation.
    pub invitation_ttl: Duration,
    /// Maximum logical participants of one Call (initiator included).
    pub max_group_participants: usize,
    /// Maximum endpoints rung per participant.
    pub max_endpoints_per_participant: usize,
    /// How long an empty media resource survives at the provider.
    pub media_empty_timeout: Duration,
    /// Regular reconciliation interval of an active media binding.
    pub reconcile_interval: Duration,
    /// How long a Call may wait for its media resource before it fails (MEDIA).
    pub media_ensure_deadline: Duration,
    /// How long an accepted transfer target may take to join before the transfer fails.
    pub transfer_join_deadline: Duration,
}

impl Default for AppCallingConfig {
    fn default() -> Self {
        Self {
            invitation_ttl: Duration::seconds(45),
            max_group_participants: 16,
            max_endpoints_per_participant: 10,
            media_empty_timeout: Duration::minutes(5),
            reconcile_interval: Duration::seconds(15),
            media_ensure_deadline: Duration::seconds(60),
            transfer_join_deadline: Duration::seconds(60),
        }
    }
}

impl AppCallingConfig {
    /// `CALLING_INVITATION_TTL_SECONDS` (10–300, default 45),
    /// `CALLING_MAX_GROUP_PARTICIPANTS` (2–64, default 16),
    /// `CALLING_MAX_ENDPOINTS_PER_PARTICIPANT` (1–50, default 10),
    /// `CALLING_MEDIA_RECONCILE_INTERVAL_SECONDS` (2–300, default 15).
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let number =
            |var: &'static str, default: i64, min: i64, max: i64| -> Result<i64, ConfigError> {
                match get(var) {
                    None => Ok(default),
                    Some(v) => v
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .filter(|n| (min..=max).contains(n))
                        .ok_or_else(|| {
                            ConfigError::invalid(
                                var,
                                format!("must be an integer between {min} and {max}"),
                            )
                        }),
                }
            };
        let defaults = Self::default();
        Ok(Self {
            invitation_ttl: Duration::seconds(number(
                "CALLING_INVITATION_TTL_SECONDS",
                45,
                10,
                300,
            )?),
            max_group_participants: number("CALLING_MAX_GROUP_PARTICIPANTS", 16, 2, 64)? as usize,
            max_endpoints_per_participant: number(
                "CALLING_MAX_ENDPOINTS_PER_PARTICIPANT",
                10,
                1,
                50,
            )? as usize,
            reconcile_interval: Duration::seconds(number(
                "CALLING_MEDIA_RECONCILE_INTERVAL_SECONDS",
                15,
                2,
                300,
            )?),
            ..defaults
        })
    }
}

/// The application-calling orchestration.
#[derive(Clone)]
pub struct AppCallService {
    calls: Arc<CallService>,
    endpoints: Arc<EndpointService>,
    principals: Arc<PrincipalResolver>,
    media: Option<Arc<dyn CallMediaProvider>>,
    credentials: MediaCredentialPolicy,
    bus: Arc<dyn RealtimeSignalBus>,
    push: Arc<dyn PushDispatcher>,
    config: AppCallingConfig,
}

impl AppCallService {
    pub fn new(
        calls: Arc<CallService>,
        endpoints: Arc<EndpointService>,
        principals: Arc<PrincipalResolver>,
        media: Option<Arc<dyn CallMediaProvider>>,
        credentials: MediaCredentialPolicy,
        bus: Arc<dyn RealtimeSignalBus>,
        config: AppCallingConfig,
    ) -> Self {
        Self {
            calls,
            endpoints,
            principals,
            media,
            credentials,
            bus,
            push: Arc::new(NoPushTransport),
            config,
        }
    }

    /// Uses `push` for push-ready ring offers (default: no push transport).
    pub fn with_push(mut self, push: Arc<dyn PushDispatcher>) -> Self {
        self.push = push;
        self
    }

    pub fn config(&self) -> &AppCallingConfig {
        &self.config
    }

    pub fn bus(&self) -> &Arc<dyn RealtimeSignalBus> {
        &self.bus
    }

    pub fn calls(&self) -> &Arc<CallService> {
        &self.calls
    }

    fn db(&self) -> &DatabaseConnection {
        self.calls.db()
    }

    /// The media provider, or CAPABILITY_UNAVAILABLE when application media is disabled.
    fn provider(&self) -> CallResult<&Arc<dyn CallMediaProvider>> {
        self.media.as_ref().ok_or(CallError::CapabilityUnavailable(
            "application media is not configured",
        ))
    }

    /// Generic capability flags of the configured provider (none when disabled).
    pub fn capabilities(&self) -> Option<crate::media::MediaProviderCapabilities> {
        self.media.as_ref().map(|m| m.capabilities())
    }
}

/// Scope of a persisted Call row.
pub(crate) fn scope_of(call: &calls::Model) -> CallScope {
    CallScope::from_parts(
        TenantId::from_uuid(call.tenant_id),
        ApplicationId::from_uuid(call.application_id),
        EnvironmentId::from_uuid(call.environment_id),
    )
}

/// A command Calling issues itself for `call` (workers, callbacks, reconciliation).
pub(crate) fn system_cmd(call: &calls::Model) -> CallCommandContext {
    CallCommandContext::system(
        scope_of(call),
        ServiceAccountId::from_uuid(call.created_by_service_account_id),
    )
}

/// Safe public summary of a Call row.
pub(crate) fn summary(call: &calls::Model) -> Value {
    serde_json::to_value(CallSummaryView::from(&super::aggregate::Call::from(
        call.clone(),
    )))
    .unwrap_or(Value::Null)
}

/// Result of an idempotent command: the response body every replay receives.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub body: Value,
    pub replayed: bool,
}

/// Signals (and push-ready offers) produced by one committed transaction, published
/// after commit.
#[derive(Default)]
pub(crate) struct PendingSignals {
    signals: Vec<SignalEnvelope>,
    pushes: Vec<(Uuid, Value)>,
}

impl PendingSignals {
    pub fn push(&mut self, endpoint_ids: Vec<Uuid>, signal: Value) {
        if !endpoint_ids.is_empty() {
            self.signals.push(SignalEnvelope {
                endpoint_ids,
                signal,
            });
        }
    }
}

impl AppCallService {
    /// Publishes committed signals. A failed publish never affects the authoritative
    /// state: endpoints recover through the API.
    pub(crate) async fn publish(&self, signals: PendingSignals) {
        for envelope in signals.signals {
            if let Err(error) = self.bus.publish(envelope).await {
                tracing::warn!(%error, "realtime signal not published (recoverable via API)");
            }
        }
        for (target, payload) in signals.pushes {
            self.push.dispatch(target, payload).await;
        }
    }

    /// The Call's latest event sequence (for signal ordering).
    pub(crate) async fn sequence<C: ConnectionTrait>(
        &self,
        db: &C,
        call_id: Uuid,
    ) -> CallResult<i64> {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COALESCE(MAX(sequence), 0)::bigint FROM call_events WHERE call_id = $1",
                vec![call_id.into()],
            ))
            .await?
            .ok_or(CallError::NotFound)?;
        Ok(row.try_get_by_index::<i64>(0)?)
    }

    /// Every endpoint involved in a Call: sessions' endpoints and invited endpoints.
    pub(crate) async fn call_endpoints<C: ConnectionTrait>(
        &self,
        db: &C,
        call_id: Uuid,
    ) -> CallResult<Vec<Uuid>> {
        let mut ids: Vec<Uuid> = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call_id))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|s| s.endpoint_id)
            .collect();
        ids.extend(
            call_invitations::Entity::find()
                .filter(call_invitations::Column::CallId.eq(call_id))
                .all(db)
                .await?
                .into_iter()
                .map(|i| i.endpoint_id),
        );
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Builds a call-level signal for every endpoint of the Call.
    pub(crate) async fn call_signal<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        kind: SignalType,
        call: &calls::Model,
        correlation_id: &str,
        data: Map<String, Value>,
    ) -> CallResult<()> {
        let sequence = self.sequence(db, call.id).await?;
        let endpoints = self.call_endpoints(db, call.id).await?;
        pending.push(
            endpoints,
            signals::build(
                kind,
                call,
                sequence,
                correlation_id,
                None,
                data,
                self.calls.now(),
            ),
        );
        Ok(())
    }

    /// Builds an invitation signal for the invitation's own endpoint.
    pub(crate) async fn invitation_signal<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        kind: SignalType,
        call: &calls::Model,
        invitation: &call_invitations::Model,
        correlation_id: &str,
    ) -> CallResult<()> {
        let sequence = self.sequence(db, call.id).await?;
        if kind == SignalType::Incoming {
            self.push_offer(db, pending, call, invitation).await?;
        }
        pending.push(
            vec![invitation.endpoint_id],
            signals::build(
                kind,
                call,
                sequence,
                correlation_id,
                Some(invitation),
                Map::new(),
                self.calls.now(),
            ),
        );
        Ok(())
    }
}

impl AppCallService {
    /// Queues the push-ready payload of a new offer when its endpoint has a push target.
    async fn push_offer<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        call: &calls::Model,
        invitation: &call_invitations::Model,
    ) -> CallResult<()> {
        let Some(target) = call_endpoints::Entity::find_by_id(invitation.endpoint_id)
            .one(db)
            .await?
            .and_then(|e| e.push_target_ref)
        else {
            return Ok(());
        };
        let caller = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .filter(call_participants::Column::Role.eq(ParticipantRole::Initiator))
            .one(db)
            .await?
            .and_then(|p| p.display_name);
        pending.pushes.push((
            target,
            signals::push_payload(call, invitation, caller.as_deref()),
        ));
        Ok(())
    }
}

/// Signal kind announcing a Call that reached `state` (terminal / ending / connected …).
pub(crate) fn state_signal(state: super::state::CallState) -> Option<SignalType> {
    use super::state::CallState::*;
    Some(match state {
        Ringing => SignalType::Ringing,
        Answered => SignalType::Answered,
        Connected => SignalType::Connected,
        Held => SignalType::Held,
        Transferring => SignalType::TransferStarted,
        Ending | Ended => SignalType::Ended,
        Declined => SignalType::Declined,
        Busy => SignalType::Busy,
        NoAnswer => SignalType::NoAnswer,
        Failed => SignalType::Failed,
        Cancelled => SignalType::Cancelled,
        _ => return None,
    })
}
