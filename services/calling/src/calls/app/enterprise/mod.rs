//! Enterprise calls (Part F §54–§62): the SAME canonical Call, routed by enterprise
//! configuration (ADR 0018).
//!
//! ```text
//! POST /v1/calls { targetExtension } ─▶ resolve extension (trusted scope) ─▶ policy
//!   ─▶ REQUESTED ─▶ AUTHORIZED (ENTERPRISE_INTERNAL_V1) ─▶ RESOLVING ─▶ ROUTING
//!      (ENTERPRISE_INTERNAL plan, PRIVATE_NETWORK route, RTC leg, attempt, media binding,
//!       enterprise call context)
//!   ─▶ media resource ─▶ DIALING ─▶ enterprise ring step (router) ─▶ RINGING
//!   ─▶ existing single-winner arbitration ─▶ ANSWERED ─▶ CONNECTED ─▶ ENDED
//! ```
//!
//! No public-network leg, Telephony command, spend reservation or provider cost exists for
//! an internal-only enterprise call. `enterprise_call_contexts` holds the entry extension
//! snapshot, the policy version relied on, the current destination, the hop budget and the
//! visited set; `enterprise_routing_steps` is the append-only routing evidence.

mod forward;
mod ivr;
mod queues;
mod recording;
mod ring_groups;
mod router;
mod sip;
mod transfer;
mod voicemail;

pub use ivr::IvrInputSource;
pub use recording::{RecordingActor, RecordingConsentView, StartOutcome};
pub use sip::{EnterpriseDialogOffer, EnterpriseKeypad, OfferScope};
pub use transfer::{ResolvedTransferTarget, TransferTarget};
pub use voicemail::{
    CallMediaCapture, CaptureDelivery, CapturePurpose, CaptureRequest, CapturedMedia,
    UnavailableCapture, CAPTURE_CONTENT_TYPE, MAX_CAPTURE_BYTES,
};

use sea_orm::{
    ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult, TransactionTrait,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{create::CreatedAppCall, AppCallService, PendingSignals};
use crate::{
    calls::{
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        authorizations::NewAuthorization,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallAuthorizationId, CallLegId, CallRouteId},
        kinds::{
            AuthorizationReason, CallKind, CommandOperation, FailureCategory, InvitationPurpose,
            LegDirection, LegKind, LegState, RouteClass,
        },
        legs::NewLeg,
        participants::DisplayName,
        routes::{NewRoute, PolicyVersion},
        service::{CommandKey, NewCall, NewMediaBinding},
        transitions::CallAction,
    },
    domain::PrincipalId,
    endpoints::CallEndpointId,
    enterprise::{
        destinations::{Destination, DestinationType},
        extensions,
        forwarding::ForwardingCondition,
        policy::{effective_policy, EffectivePolicy},
        EnterpriseError,
    },
    models::{call_legs, calls},
    platform::{context::PlatformAuthContext, principals::PrincipalReference},
    routing::{
        engine::{
            DestinationFacts, DuplicateRisk, EndpointClass, Evidence, RouteDecision,
            RouteDirection, RouteInputs, TelecomFlags,
        },
        RoutingClass,
    },
    signaling::signals::SignalType,
};

pub(crate) use router::RouteAction;

/// How enterprise routing continued after a ring round ended without an answer.
pub(crate) enum Continuation {
    /// New ring offers on the SAME Call (next ordered member, a fallback), or the Call
    /// reached a terminal state.
    Rung(calls::Model, Vec<crate::models::call_invitations::Model>),
    /// The Call just entered a queue (it waits there, RINGING, without offers).
    Queued(calls::Model),
    /// The Call keeps waiting in its queue for the next available agent.
    Waiting,
}

/// Authorization policy of enterprise internal calls.
pub const ENTERPRISE_POLICY: &str = "ENTERPRISE_INTERNAL_V1";
/// Route selection policy of enterprise calls.
pub const ENTERPRISE_ROUTE_POLICY: &str = "ENTERPRISE_ROUTE_V1";

/// `POST /v1/calls` with an extension target.
#[derive(Debug, Clone)]
pub struct CreateEnterpriseCall {
    pub kind: CallKind,
    pub initiator: PrincipalId,
    pub initiator_endpoint: CallEndpointId,
    /// The dialled extension number (resolved in the credential's scope only).
    pub extension: String,
    pub initiator_display_name: Option<DisplayName>,
    pub key: CommandKey,
}

text_enum!(
    /// Where an enterprise Call is in its routing.
    EnterprisePhase {
        Routing = "ROUTING",
        Ringing = "RINGING",
        RingGroup = "RING_GROUP",
        Queued = "QUEUED",
        Ivr = "IVR",
        Voicemail = "VOICEMAIL",
        Forwarded = "FORWARDED",
        Answered = "ANSWERED",
        Ended = "ENDED",
    }
);

/// The per-Call enterprise routing state.
#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct ContextRow {
    pub call_id: Uuid,
    pub entry_extension_id: Uuid,
    pub entry_extension_number: String,
    pub entry_label: String,
    pub policy_id: Option<Uuid>,
    pub policy_version: i64,
    pub phase: String,
    pub current_destination_type: Option<String>,
    pub current_destination_id: Option<Uuid>,
    pub hop_count: i16,
    pub max_hops: i16,
    pub visited: Vec<Uuid>,
    pub answered_principal_id: Option<Uuid>,
    pub outcome: Option<String>,
    pub ring_group_id: Option<Uuid>,
    pub ring_cursor: Option<i16>,
    pub answer_group_id: Option<Uuid>,
    pub ring_deadline: Option<chrono::DateTime<chrono::Utc>>,
    pub queue_entry_id: Option<Uuid>,
    pub ivr_session_id: Option<Uuid>,
}

impl ContextRow {
    pub(crate) fn destination(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(self.current_destination_type.as_deref()?)?,
            self.current_destination_id?,
        ))
    }
}

const CONTEXT_COLUMNS: &str = "call_id, entry_extension_id, entry_extension_number, entry_label, \
    policy_id, policy_version, phase, current_destination_type, current_destination_id, hop_count, \
    max_hops, visited, answered_principal_id, outcome, ring_group_id, ring_cursor, answer_group_id, \
    ring_deadline, queue_entry_id, ivr_session_id";

pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> sea_orm::Statement {
    sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres, sql, values)
}

pub(crate) async fn exec<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> CallResult<u64> {
    Ok(db.execute(stmt(sql, values)).await?.rows_affected())
}

/// The enterprise context of a Call, if it entered enterprise routing.
pub(crate) async fn context_of<C: ConnectionTrait>(
    db: &C,
    call_id: Uuid,
) -> CallResult<Option<ContextRow>> {
    Ok(ContextRow::find_by_statement(stmt(
        &format!("SELECT {CONTEXT_COLUMNS} FROM enterprise_call_contexts WHERE call_id = $1"),
        vec![call_id.into()],
    ))
    .one(db)
    .await?)
}

/// The first column of every row (text).
pub(crate) async fn strings_in<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> CallResult<Vec<String>> {
    db.query_all(stmt(sql, values))
        .await?
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).map_err(Into::into))
        .collect()
}

/// Appends one routing-evidence step and its durable ENTERPRISE_ROUTING_STEP Call event
/// (transactional outbox, same transaction).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_step(
    calls: &crate::calls::service::CallService,
    txn: &DatabaseTransaction,
    call: &calls::Model,
    decision: &'static str,
    destination: Option<Destination>,
    reason: &'static str,
    answer_group: Option<Uuid>,
    at: chrono::DateTime<chrono::Utc>,
) -> CallResult<()> {
    record_step_of(
        calls,
        txn,
        call.id,
        super::scope_of(call),
        decision,
        destination,
        reason,
        answer_group,
        at,
    )
    .await
}

/// [`record_step`] by Call id and scope (the router, which holds no Call model).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_step_of(
    calls: &crate::calls::service::CallService,
    txn: &DatabaseTransaction,
    call_id: Uuid,
    scope: CallScope,
    decision: &'static str,
    destination: Option<Destination>,
    reason: &'static str,
    answer_group: Option<Uuid>,
    at: chrono::DateTime<chrono::Utc>,
) -> CallResult<()> {
    let step: i64 = crate::calls::service::scalar_i64(
        txn,
        "INSERT INTO enterprise_routing_steps (call_id, step_number, tenant_id, application_id, \
         environment_id, decision, destination_type, destination_id, reason, answer_group_id, \
         decided_at) SELECT $1, COALESCE(MAX(step_number), 0) + 1, $2, $3, $4, $5, $6, $7, $8, $9, $10 \
         FROM enterprise_routing_steps WHERE call_id = $1 RETURNING step_number::bigint",
        vec![
            call_id.into(),
            scope.tenant_id().as_uuid().into(),
            scope.application_id().as_uuid().into(),
            scope.environment_id().as_uuid().into(),
            decision.into(),
            destination.map(|d| d.kind.as_str()).into(),
            destination.map(|d| d.id).into(),
            reason.into(),
            answer_group.into(),
            at.into(),
        ],
    )
    .await?;
    let call = calls::Entity::find_by_id(call_id)
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)?;
    let mut fields = vec![
        (
            "step_number",
            crate::calls::attributes::SafeValue::Integer(step),
        ),
        ("decision", crate::calls::service::text(decision)),
        ("reason", crate::calls::service::text(reason)),
    ];
    if let Some(d) = destination {
        fields.push((
            "destination_type",
            crate::calls::service::text(d.kind.as_str()),
        ));
        // A public contact is an opaque ContactReference: never repeated in an event.
        if d.kind != DestinationType::PublicContact {
            fields.push(("destination_id", crate::calls::service::text(d.id)));
        }
    }
    calls
        .append_event(
            txn,
            &super::system_cmd(&call),
            &call,
            crate::calls::service::EventDraft::fact(
                crate::calls::events::CallEventType::EnterpriseRoutingStep,
                crate::calls::service::data(fields)?,
            ),
            at,
        )
        .await?;
    crate::metrics::inc(
        "calling_enterprise_routing_decisions_total",
        &[("decision", decision), ("reason", reason)],
    );
    Ok(())
}

/// Moves the context (versioned) to `phase`, optionally naming the current destination,
/// the hop count / visited set and an outcome.
pub(crate) async fn update_context<C: ConnectionTrait>(
    db: &C,
    context: &ContextRow,
    phase: EnterprisePhase,
    destination: Option<Destination>,
    outcome: Option<&'static str>,
    at: chrono::DateTime<chrono::Utc>,
) -> CallResult<()> {
    exec(
        db,
        "UPDATE enterprise_call_contexts SET phase = $2, \
         current_destination_type = COALESCE($3, current_destination_type), \
         current_destination_id = COALESCE($4, current_destination_id), \
         outcome = COALESCE($5, outcome), version = version + 1, \
         updated_at = GREATEST(updated_at, $6) WHERE call_id = $1",
        vec![
            context.call_id.into(),
            phase.as_str().into(),
            destination.map(|d| d.kind.as_str()).into(),
            destination.map(|d| d.id).into(),
            outcome.into(),
            at.into(),
        ],
    )
    .await?;
    Ok(())
}

fn enterprise_error(e: EnterpriseError) -> CallError {
    match e {
        EnterpriseError::NotFound => CallError::NotFound,
        EnterpriseError::Validation(m) => CallError::Validation(m),
        EnterpriseError::Database {
            summary,
            connectivity,
        } => CallError::Database {
            summary,
            connectivity,
        },
        EnterpriseError::PolicyDenied(_) => CallError::AuthorizationDenied,
        EnterpriseError::Unavailable(m) => CallError::CapabilityUnavailable(m),
        EnterpriseError::StorageUnavailable => CallError::ArtifactStorageUnavailable,
        EnterpriseError::VersionConflict { .. } | EnterpriseError::Conflict(_) => {
            CallError::Conflict("enterprise configuration changed; retry")
        }
    }
}

impl AppCallService {
    /// `POST /v1/calls` to an extension: resolves the extension in the TRUSTED scope,
    /// applies the enterprise policy, creates ONE canonical Call on an ENTERPRISE_INTERNAL
    /// route and starts enterprise routing.
    pub async fn create_enterprise_call(
        &self,
        ctx: &PlatformAuthContext,
        request: CreateEnterpriseCall,
    ) -> CallResult<CreatedAppCall> {
        let scope = CallScope::of_platform(ctx);
        if let Some(prior) = self
            .calls
            .prior_command(self.db(), scope, &request.key)
            .await?
        {
            return self.replay_create(scope, prior).await;
        }
        if !self.enterprise_flags.enterprise_calling {
            return Err(CallError::CapabilityUnavailable(
                "enterprise calling is not enabled",
            ));
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application calls",
            ));
        }
        let initiator = self
            .principals
            .resolve(ctx, &PrincipalReference::Id(request.initiator))
            .await
            .map_err(|e| super::create::resolution(e, true))?;
        let initiator_endpoint = self
            .endpoints
            .active_endpoint(scope, request.initiator, request.initiator_endpoint)
            .await?;
        // The extension of THIS scope only: another tenant's / application's /
        // environment's number is invisible (namespace = scope).
        let Some(extension) = extensions::resolve_number(self.db(), scope, &request.extension)
            .await
            .map_err(enterprise_error)?
        else {
            crate::metrics::inc(
                "calling_enterprise_extension_resolution_failures_total",
                &[("reason", "NOT_FOUND")],
            );
            return Err(CallError::NotFound);
        };
        let policy = effective_policy(self.db(), scope, extension.policy_id)
            .await
            .map_err(enterprise_error)?;
        if !policy.rules.internal_calling_enabled {
            crate::metrics::inc(
                "calling_enterprise_extension_resolution_failures_total",
                &[("reason", "POLICY_DENIED")],
            );
            return Err(CallError::AuthorizationDenied);
        }
        let destination = extension.destination().map_err(enterprise_error)?;
        if destination.kind.is_principal() && destination.id == request.initiator.as_uuid() {
            return Err(CallError::Validation(
                "the extension routes to the initiator itself.",
            ));
        }

        let cmd = CallCommandContext::customer(ctx);
        let txn = self.db().begin().await?;
        if let Some(prior) = self
            .calls
            .begin_command_in(&txn, scope, &request.key)
            .await?
        {
            txn.rollback().await?;
            return self.replay_create(scope, prior).await;
        }
        let (call, initiator_participant, _) = self
            .calls
            .create_requested_call_in(
                &txn,
                ctx,
                &initiator,
                NewCall {
                    kind: request.kind,
                    communication_context_ref: None,
                    privacy_relay_ref: None,
                    initiator_display_name: request.initiator_display_name.clone(),
                },
            )
            .await?;
        self.calls
            .record_command_in(&txn, &cmd, &request.key, Some(call.id), None, None, None)
            .await?;
        let call = self
            .authorize_enterprise_in(
                &txn,
                &cmd,
                &call,
                request.kind,
                &extension,
                destination,
                &policy,
            )
            .await?;
        self.calls
            .ensure_media_permissions_in(&txn, &call, &initiator_participant)
            .await?;
        self.calls
            .insert_session(
                &txn,
                &cmd,
                &call,
                &initiator_participant,
                initiator_endpoint.kind.session_category(),
                Some(initiator_endpoint.id.as_uuid()),
                None,
            )
            .await?;
        let call = match self
            .route_enterprise_in(&txn, &cmd, &call, destination, "ENTERPRISE_EXTENSION")
            .await
        {
            Ok((call, _)) => call,
            Err(error) => {
                txn.rollback().await?;
                return Err(error);
            }
        };
        insert_context(
            &txn,
            &call,
            &extension,
            &policy,
            destination,
            self.calls.now(),
        )
        .await?;
        let call = self.locked(&txn, &call).await?;
        record_step(
            &self.calls,
            &txn,
            &call,
            "ENTER",
            Some(Destination::new(DestinationType::Extension, extension.id)),
            "EXTENSION_DIALLED",
            None,
            self.calls.now(),
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_calls_total",
            &[("route_class", "ENTERPRISE_INTERNAL")],
        );
        tracing::info!(call_id = %call.id, request_id = %cmd.request_id(), "enterprise call routed; ensuring media resource");
        self.advance_routing(call.id).await?;
        let txn = self.db().begin().await?;
        let call = crate::models::calls::Entity::find_by_id(call.id)
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        let body = self.create_response(&txn, &call).await?;
        txn.commit().await?;
        Ok(CreatedAppCall {
            status: 201,
            body,
            replayed: false,
        })
    }

    /// ENTERPRISE_INTERNAL_V1 (inside the creating transaction): the policy snapshot is
    /// recorded and the Call moves REQUESTED -> AUTHORIZED -> RESOLVING. Never a
    /// client-supplied result.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn authorize_enterprise_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        kind: CallKind,
        extension: &extensions::ExtensionRow,
        destination: Destination,
        policy: &EffectivePolicy,
    ) -> CallResult<calls::Model> {
        let mut snapshot = SafeAttributes::new();
        snapshot.insert("call_kind", SafeValue::Text(kind.to_string()))?;
        snapshot.insert("scope_rule", SafeValue::Text("SAME_ENVIRONMENT".into()))?;
        snapshot.insert("extension_id", SafeValue::Text(extension.id.to_string()))?;
        snapshot.insert(
            "destination_type",
            SafeValue::Text(destination.kind.as_str().into()),
        )?;
        snapshot.insert("enterprise_policy", SafeValue::Text(policy.label()))?;
        let mut limits = SafeAttributes::new();
        limits.insert(
            "max_routing_hops",
            SafeValue::from(policy.rules.max_routing_hops as usize),
        )?;
        limits.insert(
            "max_ring_group_fanout",
            SafeValue::from(policy.rules.max_ring_group_fanout as usize),
        )?;
        let authorization = self
            .calls
            .record_authorization_in(
                txn,
                cmd,
                call,
                NewAuthorization {
                    reason: AuthorizationReason::PolicySatisfied,
                    policy_version: PolicyVersion::parse(ENTERPRISE_POLICY)?,
                    policy_snapshot: snapshot,
                    limits_snapshot: limits,
                },
            )
            .await?;
        let call = self
            .calls
            .transition_in(
                txn,
                cmd,
                call,
                1,
                CallAction::Authorize {
                    authorization_id: CallAuthorizationId::from_uuid(authorization.id),
                },
            )
            .await?
            .call;
        Ok(self
            .calls
            .transition_in(txn, cmd, &call, call.version, CallAction::BeginResolution)
            .await?
            .call)
    }

    /// RESOLVING -> ROUTING over ENTERPRISE_INTERNAL: route (PRIVATE_NETWORK + plan), the
    /// Call's RTC application leg, its attempt and media binding. Returns the Call and
    /// the route id.
    pub(crate) async fn route_enterprise_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        destination: Destination,
        reason: &'static str,
    ) -> CallResult<(calls::Model, Uuid)> {
        let provider = self.provider()?.clone();
        let call = self.locked(txn, call).await?;
        let plan = self
            .enterprise_plan(txn, super::scope_of(&call))
            .await?
            .ok_or(CallError::CapabilityUnavailable(
                "the routing policy does not allow ENTERPRISE_INTERNAL",
            ))?;
        let call = self
            .calls
            .transition_in(txn, cmd, &call, call.version, CallAction::BeginRouting)
            .await?
            .call;
        let mut decision = SafeAttributes::new();
        decision.insert("reason", SafeValue::Text(reason.into()))?;
        decision.insert(
            "destination_type",
            SafeValue::Text(destination.kind.as_str().into()),
        )?;
        let route = self
            .calls
            .record_route_in(
                txn,
                cmd,
                &call,
                NewRoute {
                    route_class: RouteClass::PrivateNetwork,
                    policy_version: PolicyVersion::parse(ENTERPRISE_ROUTE_POLICY)?,
                    decision_snapshot: decision,
                    plan: Some(plan),
                },
            )
            .await?;
        // The internal media path is the Call's RTC resource (no carrier, no spend).
        let leg = self
            .calls
            .record_leg_in(
                txn,
                cmd,
                &call,
                NewLeg {
                    route_id: Some(CallRouteId::from_uuid(route.id)),
                    participant_id: None,
                    direction: LegDirection::Outbound,
                    kind: LegKind::Application,
                },
            )
            .await?;
        self.calls
            .record_attempt_in(
                txn,
                cmd,
                &call,
                NewAttempt {
                    route_id: Some(CallRouteId::from_uuid(route.id)),
                    leg_id: Some(CallLegId::from_uuid(leg.id)),
                    previous_attempt_id: None,
                },
            )
            .await?;
        self.calls
            .record_media_binding_in(
                txn,
                &call,
                NewMediaBinding {
                    route_id: route.id,
                    leg_id: leg.id,
                    adapter: provider.adapter_name(),
                    max_participants: (self.config.max_group_participants * 2) as i32,
                },
            )
            .await?;
        Ok((call, route.id))
    }

    /// The ENTERPRISE_INTERNAL route plan of this environment's routing policy (None if
    /// the policy does not allow the class).
    async fn enterprise_plan(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
    ) -> CallResult<Option<crate::routing::engine::RoutePlan>> {
        let policy =
            crate::routing::store::load_policy(txn, scope.environment_id().as_uuid()).await?;
        let decision = crate::routing::engine::select(
            &policy.policy,
            &RouteInputs {
                direction: RouteDirection::Outbound,
                source: EndpointClass::Enterprise,
                source_contact: None,
                destinations: vec![DestinationFacts {
                    class: EndpointClass::Enterprise,
                    app_reachable: Evidence::Unknown,
                    contact: None,
                }],
                requested: Some(RoutingClass::EnterpriseInternal),
                authorized: true,
                permitted_classes: vec![RoutingClass::EnterpriseInternal],
                relay_session_id: None,
                context_id: None,
                live_telecom_allowed: false,
                flags: TelecomFlags::default(),
                routes: Evidence::Unknown,
                spend_headroom_minor: Evidence::Unknown,
                operational_incident: Evidence::Unknown,
                duplicate_risk: DuplicateRisk::None,
            },
            self.calls.now(),
        );
        Ok(match decision {
            RouteDecision::Selected(mut plan) => {
                plan.policy_version = policy.version_label();
                Some(*plan)
            }
            RouteDecision::Denied(denial) => {
                tracing::info!(denial = denial.as_str(), "enterprise route denied");
                None
            }
        })
    }

    /// The enterprise ring step (called from `advance_routing` once the Call is DIALING
    /// on its media resource): the router turns the context's destination into ringing.
    /// Returns `Ok(None)` when the Call is not an enterprise Call.
    pub(crate) async fn enterprise_first_ring(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
    ) -> CallResult<Option<(calls::Model, Vec<crate::models::call_invitations::Model>)>> {
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok(None);
        };
        let Some(destination) = context.destination() else {
            return Ok(None);
        };
        let scope = super::scope_of(call);
        let policy = effective_policy(txn, scope, context.policy_id)
            .await
            .map_err(enterprise_error)?;
        let now = self.calls.now();
        let action = router::resolve(
            &self.calls,
            txn,
            &self.principals,
            scope,
            &policy,
            &context,
            destination,
            now,
        )
        .await?;
        let (call, offers) = self
            .apply_route_action(txn, cmd, call, leg, &context, &policy, action)
            .await?;
        if offers.is_empty()
            && !call.current_state.is_terminal()
            && !self.waiting_in(txn, &call).await?
        {
            // Nothing could be rung (e.g. a ring group without an eligible member or a
            // fallback): fail safe instead of leaving the Call DIALING.
            let context = context_of(txn, call.id).await?.unwrap_or(context);
            record_step(
                &self.calls,
                txn,
                &call,
                "NO_ROUTE",
                context.destination(),
                "NO_ELIGIBLE_TARGET",
                None,
                now,
            )
            .await?;
            return self
                .enterprise_no_route(
                    txn,
                    cmd,
                    &call,
                    &context,
                    FailureCategory::TargetUnavailable,
                )
                .await
                .map(Some);
        }
        Ok(Some((call, offers)))
    }

    /// Whether the Call legitimately waits without ring offers (queued, or in an IVR).
    pub(crate) async fn waiting_in<C: ConnectionTrait>(
        &self,
        db: &C,
        call: &calls::Model,
    ) -> CallResult<bool> {
        Ok(context_of(db, call.id).await?.is_some_and(|c| {
            c.phase == EnterprisePhase::Queued.as_str()
                || c.phase == EnterprisePhase::Ivr.as_str()
                || c.phase == EnterprisePhase::Forwarded.as_str()
                || c.phase == EnterprisePhase::Voicemail.as_str()
        }))
    }

    /// Executes a routing action on a DIALING / RINGING Call. Returns the Call as it now
    /// is and the new ring offers.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn apply_route_action(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
        context: &ContextRow,
        policy: &EffectivePolicy,
        action: RouteAction,
    ) -> CallResult<(calls::Model, Vec<crate::models::call_invitations::Model>)> {
        let now = self.calls.now();
        match action {
            RouteAction::RingPrincipal {
                principal,
                destination,
            } => {
                let (participant, _) = self
                    .calls
                    .add_participant_in(txn, cmd, call, &principal, None)
                    .await?;
                self.calls
                    .ensure_media_permissions_in(txn, call, &participant)
                    .await?;
                let endpoints = self
                    .endpoints
                    .eligible_in(
                        txn,
                        super::scope_of(call),
                        principal.id().as_uuid(),
                        self.config.max_endpoints_per_participant,
                    )
                    .await?;
                if endpoints.is_empty() {
                    // The principal's UNAVAILABLE forwarding, if any, takes the SAME Call.
                    let fresh = context_of(txn, call.id).await?.unwrap_or(context.clone());
                    if let Some(action) = router::resolve_forward(
                        &self.calls,
                        txn,
                        &self.principals,
                        super::scope_of(call),
                        policy,
                        &fresh,
                        principal.id().as_uuid(),
                        ForwardingCondition::Unavailable,
                        now,
                    )
                    .await?
                    {
                        match action {
                            // A forward that would loop / exceed the budget: evidence, then
                            // the ordinary TARGET_UNREACHABLE outcome below.
                            RouteAction::NoRoute {
                                reason,
                                destination,
                            } => {
                                record_step(
                                    &self.calls,
                                    txn,
                                    call,
                                    "NO_ROUTE",
                                    destination,
                                    reason,
                                    None,
                                    now,
                                )
                                .await?;
                            }
                            action => {
                                let fresh = context_of(txn, call.id).await?.unwrap_or(fresh);
                                return Box::pin(self.apply_route_action(
                                    txn, cmd, call, leg, &fresh, policy, action,
                                ))
                                .await;
                            }
                        }
                    }
                    record_step(
                        &self.calls,
                        txn,
                        call,
                        "NO_ROUTE",
                        Some(destination),
                        "TARGET_UNREACHABLE",
                        None,
                        now,
                    )
                    .await?;
                    return self
                        .enterprise_no_route(
                            txn,
                            cmd,
                            call,
                            context,
                            FailureCategory::TargetUnavailable,
                        )
                        .await;
                }
                let invitations = self
                    .calls
                    .issue_invitations_in(
                        txn,
                        cmd,
                        call,
                        &participant,
                        &endpoints,
                        InvitationPurpose::Call,
                        None,
                        self.config.invitation_ttl,
                        None,
                    )
                    .await?;
                let call = self.mark_ringing(txn, cmd, call, leg).await?;
                record_step(
                    &self.calls,
                    txn,
                    &call,
                    "RING_PRINCIPAL",
                    Some(destination),
                    "DESTINATION_RESOLVED",
                    None,
                    now,
                )
                .await?;
                update_context(
                    txn,
                    context,
                    EnterprisePhase::Ringing,
                    Some(destination),
                    None,
                    now,
                )
                .await?;
                Ok((call, invitations))
            }
            RouteAction::RingGroup { group, destination } => {
                // Boxed: a group's fallback may route to another group (bounded by the
                // hop budget and the visited set).
                Box::pin(self.start_ring_group(
                    txn,
                    cmd,
                    call,
                    leg,
                    context,
                    policy,
                    group,
                    destination,
                ))
                .await
            }
            RouteAction::EnterIvr { flow, destination } => {
                Box::pin(self.enter_ivr(txn, cmd, call, leg, context, policy, flow, destination))
                    .await
            }
            RouteAction::EnterQueue { queue, destination } => {
                Box::pin(self.enter_queue(txn, cmd, call, leg, context, policy, queue, destination))
                    .await
            }
            RouteAction::EnterVoicemail {
                mailbox,
                destination,
            } => {
                self.enter_voicemail(txn, cmd, call, context, mailbox, destination)
                    .await
            }
            RouteAction::ForwardPublic { destination } => {
                self.begin_public_forward_in(txn, cmd, call, context, destination)
                    .await
            }
            RouteAction::NoRoute {
                reason,
                destination,
            } => {
                record_step(
                    &self.calls,
                    txn,
                    call,
                    "NO_ROUTE",
                    destination,
                    reason,
                    None,
                    now,
                )
                .await?;
                let category = if reason == "TARGET_UNAVAILABLE" {
                    FailureCategory::TargetUnavailable
                } else {
                    FailureCategory::Routing
                };
                self.enterprise_no_route(txn, cmd, call, context, category)
                    .await
            }
        }
    }

    /// DIALING → RINGING (leg ringing too); a Call already RINGING stays.
    pub(crate) async fn mark_ringing(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        leg: &call_legs::Model,
    ) -> CallResult<calls::Model> {
        let call = self.locked(txn, call).await?;
        if call.current_state != crate::calls::state::CallState::Dialing {
            return Ok(call);
        }
        let leg = crate::calls::service::leg_of(txn, &call, CallLegId::from_uuid(leg.id)).await?;
        if leg.state.can_transition_to(LegState::Ringing) {
            self.calls
                .move_leg_in(txn, cmd, &call, leg, LegState::Ringing, None)
                .await?;
        }
        Ok(self
            .calls
            .transition_in(txn, cmd, &call, call.version, CallAction::MarkRinging)
            .await?
            .call)
    }

    /// No usable destination: the Call fails with the given category (never a fake
    /// public-network fallback).
    async fn enterprise_no_route(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        context: &ContextRow,
        category: FailureCategory,
    ) -> CallResult<(calls::Model, Vec<crate::models::call_invitations::Model>)> {
        let call = self.locked(txn, call).await?;
        let call = self
            .calls
            .transition_in(txn, cmd, &call, call.version, CallAction::Fail { category })
            .await?
            .call;
        update_context(
            txn,
            context,
            EnterprisePhase::Ended,
            None,
            Some("FAILED"),
            self.calls.now(),
        )
        .await?;
        Ok((call, Vec::new()))
    }

    /// Enterprise bookkeeping when a ring offer of `call` was accepted by `principal`:
    /// the context records who answered (history continuity).
    pub(crate) async fn enterprise_on_answer_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        principal_id: Uuid,
    ) -> CallResult<()> {
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok(());
        };
        if context.answered_principal_id.is_some() {
            return Ok(());
        }
        if context.phase == EnterprisePhase::Queued.as_str() {
            self.queue_answered_in(txn, &context, principal_id).await?;
        }
        let now = self.calls.now();
        exec(
            txn,
            "UPDATE enterprise_call_contexts SET phase = 'ANSWERED', answered_principal_id = $2, \
             outcome = 'ANSWERED', version = version + 1, updated_at = GREATEST(updated_at, $3) \
             WHERE call_id = $1",
            vec![call.id.into(), principal_id.into(), now.into()],
        )
        .await?;
        record_step(
            &self.calls,
            txn,
            call,
            "ANSWERED",
            Some(Destination::new(DestinationType::Principal, principal_id)),
            "MEMBER_ANSWERED",
            None,
            now,
        )
        .await?;
        Ok(())
    }

    /// Records the enterprise outcome of a Call that just reached a terminal state
    /// through the ordinary ring outcome (no answer / all declined).
    pub(crate) async fn enterprise_ring_ended_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<()> {
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok(());
        };
        let outcome = match call.current_state {
            crate::calls::state::CallState::Declined => "DECLINED",
            crate::calls::state::CallState::NoAnswer => "NO_ANSWER",
            _ => return Ok(()),
        };
        if context.outcome.is_some() {
            return Ok(());
        }
        let now = self.calls.now();
        update_context(
            txn,
            &context,
            EnterprisePhase::Ended,
            None,
            Some(outcome),
            now,
        )
        .await?;
        record_step(&self.calls, txn, call, "ENDED", None, outcome, None, now).await?;
        Ok(())
    }

    /// The safe enterprise block of a Call's representation (None for other Calls).
    pub(crate) async fn enterprise_view<C: ConnectionTrait>(
        &self,
        db: &C,
        call_id: Uuid,
    ) -> CallResult<Option<Value>> {
        let Some(c) = context_of(db, call_id).await? else {
            return Ok(None);
        };
        let mut view = json!({
            "extensionId": c.entry_extension_id,
            "extensionNumber": c.entry_extension_number,
            "extensionLabel": c.entry_label,
            "policyVersion": c.policy_version,
            "phase": c.phase,
            "destinationType": c.current_destination_type,
            "outcome": c.outcome,
        });
        if let Some(entry) = c.queue_entry_id {
            if let Some(row) = db
                .query_one(stmt(
                    "SELECT e.queue_id, e.state, e.entered_at, CASE WHEN e.state IN ('WAITING', 'OFFERING') \
                       THEN (SELECT count(*) FROM enterprise_queue_entries o \
                              WHERE o.queue_id = e.queue_id AND o.state IN ('WAITING', 'OFFERING') \
                                AND (o.entered_at, o.seq) <= (e.entered_at, e.seq)) END AS position \
                       FROM enterprise_queue_entries e WHERE e.id = $1",
                    vec![entry.into()],
                ))
                .await?
            {
                let queue_id: Uuid = row.try_get("", "queue_id")?;
                let state: String = row.try_get("", "state")?;
                let entered: chrono::DateTime<chrono::Utc> = row.try_get("", "entered_at")?;
                let position: Option<i64> = row.try_get("", "position")?;
                view["queue"] = json!({
                    "queueId": queue_id,
                    "state": state,
                    "position": position,
                    "enteredAt": entered.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                });
            }
        }
        if let Some(session) = c.ivr_session_id {
            if let Some(ivr) = self.ivr_view(db, session).await? {
                view["ivr"] = ivr;
            }
        }
        Ok(Some(view))
    }

    /// Enterprise bookkeeping of a Call that just ended before it was answered by a path
    /// outside enterprise routing (the caller cancelled, a failure): the business outcome
    /// is recorded once and a queue entry gets its final state. Returns the queue whose
    /// waiting callers' positions changed.
    pub(crate) async fn enterprise_call_ended_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
    ) -> CallResult<Option<Uuid>> {
        use crate::calls::state::CallState;
        let Some(context) = context_of(txn, call.id).await? else {
            return Ok(None);
        };
        let mut queue = None;
        if context.phase == EnterprisePhase::Queued.as_str()
            && self.queue_call_ended_in(txn, call, &context).await?
        {
            queue = context.destination().map(|d| d.id);
        }
        // The IVR had served the caller (its HANGUP prompt was playing) when the Call ended.
        let served_by_ivr = context.phase == EnterprisePhase::Ivr.as_str()
            && self.ivr_session_ended_in(txn, call, &context).await?;
        if context.outcome.is_some() || context.answered_principal_id.is_some() {
            return Ok(queue);
        }
        let outcome = match call.current_state {
            _ if served_by_ivr => "COMPLETED_IN_IVR",
            // The caller left (or the Call ended) while the mailbox was taking the message.
            _ if context.phase == EnterprisePhase::Voicemail.as_str() => "VOICEMAIL",
            CallState::Cancelled => "ABANDONED",
            CallState::Failed => "FAILED",
            CallState::NoAnswer => "NO_ANSWER",
            CallState::Declined => "DECLINED",
            CallState::Busy => "BUSY",
            _ => return Ok(queue),
        };
        let now = self.calls.now();
        update_context(
            txn,
            &context,
            EnterprisePhase::Ended,
            None,
            Some(outcome),
            now,
        )
        .await?;
        record_step(&self.calls, txn, call, "ENDED", None, outcome, None, now).await?;
        crate::metrics::inc(
            "calling_enterprise_call_outcomes_total",
            &[("outcome", outcome)],
        );
        Ok(queue)
    }

    /// Signals of a committed enterprise ring step.
    pub(crate) async fn enterprise_ring_signals<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        call: &calls::Model,
        invitations: &[crate::models::call_invitations::Model],
        correlation: &str,
    ) -> CallResult<()> {
        for invitation in invitations {
            self.invitation_signal(
                db,
                pending,
                SignalType::Incoming,
                call,
                invitation,
                correlation,
            )
            .await?;
        }
        if let Some(kind) = super::state_signal(call.current_state) {
            self.call_signal(db, pending, kind, call, correlation, Map::new())
                .await?;
        }
        if invitations.is_empty() {
            self.queued_signal(db, pending, call, correlation).await?;
            self.ivr_waiting_signal(db, pending, call, correlation)
                .await?;
        }
        Ok(())
    }

    /// `call.queued` to the caller of a Call that waits in a queue (with its position).
    pub(crate) async fn queued_signal<C: ConnectionTrait>(
        &self,
        db: &C,
        pending: &mut PendingSignals,
        call: &calls::Model,
        correlation: &str,
    ) -> CallResult<()> {
        let Some(view) = self.enterprise_view(db, call.id).await? else {
            return Ok(());
        };
        let Some(queue) = view.get("queue").filter(|_| view["phase"] == "QUEUED") else {
            return Ok(());
        };
        let mut data = Map::new();
        data.insert("queueId".into(), queue["queueId"].clone());
        if let Some(position) = queue["position"].as_i64() {
            data.insert("position".into(), json!(position));
        }
        self.initiator_signal(db, pending, SignalType::Queued, call, correlation, data)
            .await
    }
}

async fn insert_context<C: ConnectionTrait>(
    db: &C,
    call: &calls::Model,
    extension: &extensions::ExtensionRow,
    policy: &EffectivePolicy,
    destination: Destination,
    at: chrono::DateTime<chrono::Utc>,
) -> CallResult<()> {
    exec(
        db,
        "INSERT INTO enterprise_call_contexts (call_id, tenant_id, application_id, environment_id, \
         entry_extension_id, entry_extension_number, entry_label, policy_id, policy_version, phase, \
         current_destination_type, current_destination_id, hop_count, max_hops, visited, version, \
         created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ROUTING', $10, $11, 0, \
         $12, ARRAY[$5]::uuid[], 1, $13, $13)",
        vec![
            call.id.into(),
            call.tenant_id.into(),
            call.application_id.into(),
            call.environment_id.into(),
            extension.id.into(),
            extension.extension_number.clone().into(),
            extension.display_label.clone().into(),
            policy.policy_id.into(),
            policy.version.into(),
            destination.kind.as_str().into(),
            destination.id.into(),
            (policy.rules.max_routing_hops as i16).into(),
            at.into(),
        ],
    )
    .await?;
    Ok(())
}

/// The bounded, safe create-call `enterprise` block of an enterprise Call.
pub(crate) fn create_block(view: Option<Value>, body: &mut Value) {
    if let (Some(view), Some(obj)) = (view, body.as_object_mut()) {
        obj.insert("enterprise".to_string(), view);
    }
}

/// Completes the create-call command of an enterprise Call whose first ring step ran.
pub(crate) async fn complete_create(
    service: &AppCallService,
    txn: &DatabaseTransaction,
    call: &calls::Model,
) -> CallResult<()> {
    let body = service.create_response(txn, call).await?;
    service
        .calls
        .complete_call_command_in(
            txn,
            call.id,
            CommandOperation::CreateCall,
            201,
            &body,
            Some(call.version),
        )
        .await
}
