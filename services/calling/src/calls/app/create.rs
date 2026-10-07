//! Creating an application call (Part C §22, §26–§30, §21 route orchestration).

use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{scope_of, summary, system_cmd, AppCallService, PendingSignals};
use crate::{
    calls::{
        attempts::NewAttempt,
        attributes::{SafeAttributes, SafeValue},
        authorizations::NewAuthorization,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        ids::{CallId, CallLegId, CallRouteId},
        kinds::{
            AttemptOutcome, AuthorizationReason, CallKind, CommandOperation, FailureCategory,
            InvitationPurpose, LegDirection, LegKind, LegState, MediaSessionStatus,
            ParticipantRole, RouteClass, RouteState,
        },
        legs::NewLeg,
        participants::DisplayName,
        routes::{NewRoute, PolicyVersion},
        service::{backoff, leg_of, route_of, CommandKey, NewCall, NewMediaBinding, PriorCommand},
        state::CallState,
        transitions::CallAction,
    },
    domain::PrincipalId,
    endpoints::CallEndpointId,
    media::provider::{MediaResourceName, MediaSessionRef, MediaSessionSpec},
    models::{call_attempts, call_invitations, call_participants, call_sessions, calls},
    platform::{
        context::PlatformAuthContext,
        principals::{PrincipalReference, PrincipalResolutionError, ResolvedPrincipal},
    },
    signaling::signals::SignalType,
};

/// Authorization policy of direct application calls.
pub const APP_DIRECT_POLICY: &str = "APP_DIRECT_V1";
/// Route selection policy of application calls.
pub const APP_ROUTE_POLICY: &str = "APP_ROUTE_V1";

/// A create-call request: CF Meet concepts only. Scope, state, authorization, route and
/// media resource are never inputs.
#[derive(Debug, Clone)]
pub struct CreateAppCall {
    pub kind: CallKind,
    pub initiator: PrincipalId,
    pub initiator_endpoint: CallEndpointId,
    pub targets: Vec<PrincipalId>,
    pub initiator_display_name: Option<DisplayName>,
    pub key: CommandKey,
}

#[derive(Debug, Clone)]
pub struct CreatedAppCall {
    pub status: u16,
    pub body: Value,
    pub replayed: bool,
}

fn resolution(err: PrincipalResolutionError, initiator: bool) -> CallError {
    match err {
        PrincipalResolutionError::NotFound => CallError::NotFound,
        PrincipalResolutionError::Inactive if initiator => CallError::AuthorizationDenied,
        PrincipalResolutionError::Inactive => CallError::TargetUnavailable,
        PrincipalResolutionError::InvalidReference => {
            CallError::Validation("A principal reference is invalid.")
        }
        PrincipalResolutionError::Unavailable { summary } => CallError::Database {
            summary,
            connectivity: true,
        },
    }
}

impl AppCallService {
    /// `POST /v1/calls`: create, authorize, resolve, route and ring an application call.
    pub async fn create_app_call(
        &self,
        ctx: &PlatformAuthContext,
        request: CreateAppCall,
    ) -> CallResult<CreatedAppCall> {
        let scope = CallScope::of_platform(ctx);
        // Replays are answered before anything else (no provider, no validation drift).
        if let Some(prior) = self
            .calls
            .prior_command(self.db(), scope, &request.key)
            .await?
        {
            return self.replay_create(scope, prior).await;
        }
        let provider = self.provider()?;
        if !provider.capabilities().supports_app_calls() {
            return Err(CallError::CapabilityUnavailable(
                "the media provider cannot execute application calls",
            ));
        }
        if request.targets.is_empty() {
            return Err(CallError::Validation(
                "targetPrincipalIds must not be empty.",
            ));
        }
        if request.targets.len() + 1 > self.config.max_group_participants {
            return Err(CallError::ParticipantLimitExceeded {
                limit: self.config.max_group_participants as i64,
            });
        }
        let mut seen = request.targets.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != request.targets.len() || request.targets.contains(&request.initiator) {
            return Err(CallError::Validation(
                "targetPrincipalIds must be distinct and must not include the initiator.",
            ));
        }
        let initiator = self
            .principals
            .resolve(ctx, &PrincipalReference::Id(request.initiator))
            .await
            .map_err(|e| resolution(e, true))?;
        let mut targets: Vec<ResolvedPrincipal> = Vec::with_capacity(request.targets.len());
        for target in &request.targets {
            targets.push(
                self.principals
                    .resolve(ctx, &PrincipalReference::Id(*target))
                    .await
                    .map_err(|e| resolution(e, false))?,
            );
        }
        let initiator_endpoint = self
            .endpoints
            .active_endpoint(scope, request.initiator, request.initiator_endpoint)
            .await?;

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

        // Authorization for this scope (APP_DIRECT_V1): authenticated application with
        // calls.write, initiator and every target resolved ACTIVE in the caller's own
        // scope, a supported kind, the concurrent-call limit passed (above), the group
        // limit respected. Never a client-supplied result.
        let mut policy = SafeAttributes::new();
        policy.insert("call_kind", SafeValue::Text(request.kind.to_string()))?;
        policy.insert("target_count", SafeValue::from(targets.len()))?;
        policy.insert("scope_rule", SafeValue::Text("SAME_ENVIRONMENT".into()))?;
        let mut limits = SafeAttributes::new();
        limits.insert(
            "max_group_participants",
            SafeValue::from(self.config.max_group_participants),
        )?;
        let authorization = self
            .calls
            .record_authorization_in(
                &txn,
                &cmd,
                &call,
                NewAuthorization {
                    reason: AuthorizationReason::PolicySatisfied,
                    policy_version: PolicyVersion::parse(APP_DIRECT_POLICY)?,
                    policy_snapshot: policy,
                    limits_snapshot: limits,
                },
            )
            .await?;
        let call = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                1,
                CallAction::Authorize {
                    authorization_id: crate::calls::ids::CallAuthorizationId::from_uuid(
                        authorization.id,
                    ),
                },
            )
            .await?
            .call;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginResolution)
            .await?
            .call;

        // Resolution: participants, server-derived media permissions, the initiator's
        // session on its endpoint, and the eligible endpoints of every target.
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
        let mut endpoint_count = 0usize;
        let mut unreachable = 0usize;
        for target in &targets {
            let (participant, _) = self
                .calls
                .add_participant_in(&txn, &cmd, &call, target, None)
                .await?;
            self.calls
                .ensure_media_permissions_in(&txn, &call, &participant)
                .await?;
            let endpoints = self
                .endpoints
                .eligible_in(
                    &txn,
                    scope,
                    target.id().as_uuid(),
                    self.config.max_endpoints_per_participant,
                )
                .await?;
            if endpoints.is_empty() {
                unreachable += 1;
            }
            endpoint_count += endpoints.len();
        }
        let call = self.locked(&txn, &call).await?;
        if unreachable > 0 {
            // Route unavailable: every target is required and only APPLICATION exists
            // today. No fake public-network fallback.
            let call = self
                .calls
                .transition_in(
                    &txn,
                    &cmd,
                    &call,
                    call.version,
                    CallAction::Fail {
                        category: FailureCategory::Routing,
                    },
                )
                .await?
                .call;
            let body = self.create_response(&txn, &call).await?;
            self.calls
                .complete_call_command_in(
                    &txn,
                    call.id,
                    CommandOperation::CreateCall,
                    201,
                    &body,
                    Some(call.version),
                )
                .await?;
            txn.commit().await?;
            tracing::info!(call_id = %call.id, unreachable, "application call route unavailable");
            let mut pending = PendingSignals::default();
            self.call_signal(
                self.db(),
                &mut pending,
                SignalType::Failed,
                &call,
                cmd.correlation_id().as_str(),
                Map::new(),
            )
            .await?;
            self.publish(pending).await;
            return Ok(CreatedAppCall {
                status: 201,
                body,
                replayed: false,
            });
        }
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginRouting)
            .await?
            .call;
        // Route orchestration (§21): one APPLICATION route, one shared RTC leg, attempt 1
        // and the private media binding under the canonical resource name.
        let mut decision = SafeAttributes::new();
        decision.insert(
            "reason",
            SafeValue::Text("ALL_TARGETS_HAVE_APPLICATION_ENDPOINTS".into()),
        )?;
        decision.insert("target_count", SafeValue::from(targets.len()))?;
        decision.insert("endpoint_count", SafeValue::from(endpoint_count))?;
        let route = self
            .calls
            .record_route_in(
                &txn,
                &cmd,
                &call,
                NewRoute {
                    route_class: RouteClass::Application,
                    policy_version: PolicyVersion::parse(APP_ROUTE_POLICY)?,
                    decision_snapshot: decision,
                },
            )
            .await?;
        let leg = self
            .calls
            .record_leg_in(
                &txn,
                &cmd,
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
                &txn,
                &cmd,
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
                &txn,
                &call,
                NewMediaBinding {
                    route_id: route.id,
                    leg_id: leg.id,
                    adapter: provider.adapter_name(),
                    max_participants: (self.config.max_group_participants * 2) as i32,
                },
            )
            .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            request_id = %cmd.request_id(),
            "application call routed; ensuring media resource"
        );

        // Media resource, then ringing (another instance's reconciler resumes it if this
        // request is interrupted).
        self.advance_routing(call.id).await?;
        let txn = self.db().begin().await?;
        let call = calls::Entity::find_by_id(call.id)
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

    /// Re-reads a locked call row inside `txn` (after `*_in` primitives touched it).
    pub(crate) async fn locked<C: ConnectionTrait>(
        &self,
        db: &C,
        call: &calls::Model,
    ) -> CallResult<calls::Model> {
        calls::Entity::find_by_id(call.id)
            .one(db)
            .await?
            .ok_or(CallError::NotFound)
    }

    async fn replay_create(
        &self,
        scope: CallScope,
        prior: PriorCommand,
    ) -> CallResult<CreatedAppCall> {
        match prior {
            PriorCommand::Completed { status, body, .. } => Ok(CreatedAppCall {
                status,
                body,
                replayed: true,
            }),
            PriorCommand::InProgress { call_id } => {
                let call_id = call_id.ok_or(CallError::NotFound)?;
                let call = calls::Entity::find_by_id(call_id)
                    .filter(calls::Column::TenantId.eq(scope.tenant_id().as_uuid()))
                    .one(self.db())
                    .await?
                    .ok_or(CallError::NotFound)?;
                Ok(CreatedAppCall {
                    status: 201,
                    body: self.create_response(self.db(), &call).await?,
                    replayed: true,
                })
            }
        }
    }

    /// The bounded create-call response: Call summary, participants, the initiator's
    /// session and the number of live ring offers.
    pub(crate) async fn create_response<C: ConnectionTrait>(
        &self,
        db: &C,
        call: &calls::Model,
    ) -> CallResult<Value> {
        let participants = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .order_by_asc(call_participants::Column::CreatedAt)
            .order_by_asc(call_participants::Column::Id)
            .all(db)
            .await?;
        let initiator = participants
            .iter()
            .find(|p| p.role == ParticipantRole::Initiator)
            .ok_or(CallError::NotFound)?;
        let session = call_sessions::Entity::find()
            .filter(call_sessions::Column::ParticipantId.eq(initiator.id))
            .order_by_asc(call_sessions::Column::CreatedAt)
            .one(db)
            .await?;
        let live = call_invitations::Entity::find()
            .filter(call_invitations::Column::CallId.eq(call.id))
            .all(db)
            .await?
            .into_iter()
            .filter(|i| i.status.is_live())
            .count();
        Ok(json!({
            "call": summary(call),
            "initiatorParticipantId": initiator.id,
            "initiatorSessionId": session.map(|s| s.id),
            "participants": participants.iter().map(|p| json!({
                "participantId": p.id,
                "principalId": p.principal_id,
                "role": p.role.as_str(),
            })).collect::<Vec<_>>(),
            "liveInvitationCount": live,
        }))
    }

    /// ROUTING → DIALING → RINGING once the provider confirmed the Call's media resource:
    /// activates the binding and the route, authorizes the initiator's media, issues one
    /// invitation per eligible endpoint and completes the creation command. Idempotent and
    /// safe to run from any instance (the reconciler resumes interrupted creations).
    pub(crate) async fn advance_routing(&self, call_id: Uuid) -> CallResult<()> {
        let provider = self.provider()?.clone();
        let txn = self.db().begin().await?;
        let call = calls::Entity::find_by_id(call_id)
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        let binding = self.calls.media_binding_in(&txn, call.id).await?;
        txn.commit().await?;
        let Some(binding) = binding else {
            return Ok(());
        };
        if call.current_state != CallState::Routing
            || binding.status != MediaSessionStatus::Ensuring
        {
            return Ok(());
        }
        let session = MediaSessionRef {
            name: MediaResourceName::for_call(CallId::from_uuid(call.id)),
        };
        let ensured = provider
            .create_or_ensure_session(
                &session,
                &MediaSessionSpec {
                    max_participants: binding.max_participants as u32,
                    empty_timeout: self.config.media_empty_timeout,
                },
            )
            .await;
        let cmd = system_cmd(&call);
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(&call), CallId::from_uuid(call.id))
            .await?;
        let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? else {
            return Ok(());
        };
        if call.current_state != CallState::Routing
            || binding.status != MediaSessionStatus::Ensuring
        {
            txn.commit().await?;
            return Ok(());
        }
        let resource_ref = match ensured {
            Ok(ensured) => ensured.resource_ref,
            Err(error) => {
                let binding = self
                    .calls
                    .record_media_failure_in(
                        &txn,
                        &cmd,
                        &call,
                        binding,
                        "ENSURE_RESOURCE",
                        error.failure(),
                    )
                    .await?;
                let attempt = self.pending_attempt(&txn, &call, binding.route_id).await?;
                if let Some(attempt) = attempt {
                    if error == crate::media::MediaProviderError::Timeout {
                        // The provider may or may not have created it: unknown, not failed.
                        self.calls
                            .finish_attempt_in(
                                &txn,
                                &cmd,
                                &call,
                                attempt,
                                AttemptOutcome::Unknown,
                                None,
                            )
                            .await?;
                    }
                }
                let routing_since = call.routing_at.unwrap_or(call.updated_at);
                let mut pending = PendingSignals::default();
                if self.calls.now() - routing_since >= self.config.media_ensure_deadline {
                    let call = self
                        .calls
                        .transition_in(
                            &txn,
                            &cmd,
                            &call,
                            call.version,
                            CallAction::Fail {
                                category: FailureCategory::Media,
                            },
                        )
                        .await?
                        .call;
                    let body = self.create_response(&txn, &call).await?;
                    self.calls
                        .complete_call_command_in(
                            &txn,
                            call.id,
                            CommandOperation::CreateCall,
                            201,
                            &body,
                            Some(call.version),
                        )
                        .await?;
                    self.call_signal(
                        &txn,
                        &mut pending,
                        SignalType::Failed,
                        &call,
                        cmd.correlation_id().as_str(),
                        Map::new(),
                    )
                    .await?;
                }
                txn.commit().await?;
                self.publish(pending).await;
                tracing::warn!(call_id = %call.id, failure = %error, retry_in_s = backoff(binding.failure_count).num_seconds(), "media resource not ensured; will retry");
                return Ok(());
            }
        };
        let next = self.calls.now() + self.config.reconcile_interval;
        let binding = self
            .calls
            .activate_media_binding_in(&txn, &cmd, &call, binding, resource_ref, next)
            .await?;
        let route = route_of(&txn, &call, CallRouteId::from_uuid(binding.route_id)).await?;
        if route.state == RouteState::Selected {
            self.calls
                .move_route_in(&txn, &cmd, &call, route, RouteState::Active)
                .await?;
        }
        let leg = leg_of(&txn, &call, CallLegId::from_uuid(binding.leg_id)).await?;
        let leg = self
            .calls
            .move_leg_in(&txn, &cmd, &call, leg, LegState::Dialing, None)
            .await?;
        let call = self
            .calls
            .transition_in(&txn, &cmd, &call, call.version, CallAction::BeginDialing)
            .await?
            .call;
        // The initiator may join the media resource from now on.
        for session in call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .all(&txn)
            .await?
        {
            let participant = call_participants::Entity::find_by_id(session.participant_id)
                .one(&txn)
                .await?
                .ok_or(CallError::NotFound)?;
            if participant.role == ParticipantRole::Initiator {
                self.calls.authorize_session_media_in(&txn, session).await?;
            }
        }
        // Ring every target on its eligible endpoints.
        let mut invitations = Vec::new();
        for participant in call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .filter(call_participants::Column::Role.eq(ParticipantRole::Recipient))
            .filter(call_participants::Column::EndedAt.is_null())
            .all(&txn)
            .await?
        {
            let endpoints = self
                .endpoints
                .eligible_in(
                    &txn,
                    scope_of(&call),
                    participant.principal_id,
                    self.config.max_endpoints_per_participant,
                )
                .await?;
            invitations.extend(
                self.calls
                    .issue_invitations_in(
                        &txn,
                        &cmd,
                        &call,
                        &participant,
                        &endpoints,
                        InvitationPurpose::Call,
                        None,
                        self.config.invitation_ttl,
                    )
                    .await?,
            );
        }
        let mut pending = PendingSignals::default();
        let call = if invitations.is_empty() {
            // Every endpoint disappeared between resolution and ringing.
            self.calls
                .transition_in(
                    &txn,
                    &cmd,
                    &call,
                    call.version,
                    CallAction::Fail {
                        category: FailureCategory::Routing,
                    },
                )
                .await?
                .call
        } else {
            self.calls
                .move_leg_in(&txn, &cmd, &call, leg, LegState::Ringing, None)
                .await?;
            self.calls
                .transition_in(&txn, &cmd, &call, call.version, CallAction::MarkRinging)
                .await?
                .call
        };
        let body = self.create_response(&txn, &call).await?;
        self.calls
            .complete_call_command_in(
                &txn,
                call.id,
                CommandOperation::CreateCall,
                201,
                &body,
                Some(call.version),
            )
            .await?;
        let correlation = cmd.correlation_id().as_str().to_string();
        for invitation in &invitations {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::Incoming,
                &call,
                invitation,
                &correlation,
            )
            .await?;
        }
        if let Some(kind) = super::state_signal(call.current_state) {
            self.call_signal(&txn, &mut pending, kind, &call, &correlation, Map::new())
                .await?;
        }
        txn.commit().await?;
        self.publish(pending).await;
        tracing::info!(call_id = %call.id, invitations = invitations.len(), call_state = %call.current_state, "application call ringing");
        Ok(())
    }

    /// The pending attempt on `route_id`, if any.
    pub(crate) async fn pending_attempt<C: ConnectionTrait>(
        &self,
        db: &C,
        call: &calls::Model,
        route_id: Uuid,
    ) -> CallResult<Option<call_attempts::Model>> {
        let attempt = call_attempts::Entity::find()
            .filter(call_attempts::Column::CallId.eq(call.id))
            .filter(call_attempts::Column::RouteId.eq(route_id))
            .filter(
                call_attempts::Column::Outcome
                    .is_in([AttemptOutcome::Pending, AttemptOutcome::Unknown]),
            )
            .order_by_desc(call_attempts::Column::AttemptNumber)
            .one(db)
            .await?;
        Ok(attempt)
    }
}
