//! Media authority at runtime (Part C §42–§50, §83, §84): join credentials, admission of
//! provider connections, authenticated provider callbacks, removal enforcement and
//! reconciliation of one Call.
//!
//! Stale re-entry prevention (§44), layered:
//!
//! 1. every session has its own server-generated media identity; a removed / ended
//!    session's identity is never reissued, and its media access is REVOKED for good;
//! 2. join credentials grant joining only (no publish, subscribe or data); a connection
//!    receives media permissions only after Calling verified — from the database — that
//!    its identity is a current, authorized session AND that it carries the session's
//!    latest credential generation (`cfm.gen`); anything else is evicted;
//! 3. removal strips the connection's permissions first (the provider then hands the
//!    client a grant-less token), then disconnects it; failures stay pending and are
//!    retried by reconciliation, never rolled back into access;
//! 4. every provider join callback and every reconciliation pass re-applies (2), so a
//!    stale credential that re-joins is evicted again.

use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait,
};
use serde_json::Map;
use uuid::Uuid;

use super::{scope_of, system_cmd, AppCallService, PendingSignals};
use crate::{
    calls::{
        attributes::SafeValue,
        command::CallScope,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallId, CallLegId, CallSessionId, MediaProviderEventId},
        kinds::{
            DisconnectReason, LegState, MediaAccess, MediaEventKind, MediaEventOutcome,
            MediaIdentityStatus, MediaSessionStatus, SessionState,
        },
        service::leg_of,
        state::CallState,
        transitions::CallAction,
    },
    media::{
        events::{admission, Admission, EvictionReason, SessionMediaFacts},
        gateway::{ConnectionAdmission, ConnectionClaims, ConnectionDenied},
        permissions::media_state,
        provider::{
            EventVerificationError, MediaCredentialRequest, MediaParticipantRef, MediaResourceName,
            MediaSessionRef, ProviderParticipant,
        },
        reconciliation::{plan, ExpectedSession, RepairAction},
        MediaCredentialView, MediaPermissions,
    },
    models::{call_media_provider_events, call_participants, call_sessions, calls},
    signaling::{signals::SignalType, SignalingIdentity},
};

/// Who asks for a media credential.
#[derive(Debug, Clone)]
pub enum MediaCredentialActor {
    /// The application backend (API credential with calls.write), in its scope.
    Backend(crate::calls::command::CallCommandContext),
    /// The endpoint itself (signaling credential): only its own sessions.
    Endpoint(SignalingIdentity),
}

/// What processing a verified callback did (operational, never public).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackResult {
    Processed(MediaEventOutcome),
    Duplicate,
    NotOurs,
}

fn resource(call_id: Uuid) -> MediaSessionRef {
    MediaSessionRef {
        name: MediaResourceName::for_call(CallId::from_uuid(call_id)),
    }
}

impl AppCallService {
    /// Effective permissions of one participant now (authorized ∧ kind ∧ Call state,
    /// with the hold mask for a TRANSFERRING Call that was HELD before).
    pub(crate) async fn effective_permissions(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        participant_id: Uuid,
    ) -> CallResult<MediaPermissions> {
        let authorized = self
            .calls
            .authorized_permissions_in(txn, call, participant_id)
            .await?;
        let held_before = match self.calls.active_transfer(txn, call).await? {
            Some(t) => t.pre_transfer_state == CallState::Held,
            None => false,
        };
        Ok(MediaPermissions::effective(
            authorized,
            call.call_kind,
            call.current_state,
            held_before,
        ))
    }

    /// `POST …/media-credentials` (§54, §83): a short-lived join credential for an
    /// authorized session — after its invitation won (recipients), once the media route
    /// exists (initiator), never for a removed / ended session, a terminal Call or a
    /// foreign participant. Advances the session's media generation (superseding every
    /// earlier credential) and records the issuance (never the credential).
    pub async fn issue_media_credential(
        &self,
        actor: MediaCredentialActor,
        call_id: Option<CallId>,
        session_id: CallSessionId,
    ) -> CallResult<MediaCredentialView> {
        let provider = self.provider()?.clone();
        if !provider.capabilities().credential_issuance {
            return Err(CallError::CapabilityUnavailable(
                "media credentials are not available",
            ));
        }
        let cmd = match &actor {
            MediaCredentialActor::Backend(cmd) => cmd.clone(),
            MediaCredentialActor::Endpoint(identity) => {
                crate::calls::command::CallCommandContext::endpoint(identity)
            }
        };
        let scope: CallScope = cmd.scope();
        let txn = self.db().begin().await?;
        let located = call_sessions::Entity::find_by_id(session_id.as_uuid())
            .filter(call_sessions::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(call_sessions::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(call_sessions::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        if call_id.is_some_and(|c| c.as_uuid() != located.call_id) {
            return Err(CallError::NotFound);
        }
        let call = self
            .calls
            .lock_call(&txn, scope, CallId::from_uuid(located.call_id))
            .await?;
        let session = crate::calls::service::session_of(&txn, &call, session_id).await?;
        let participant = crate::calls::service::participant_of(
            &txn,
            &call,
            crate::calls::ids::CallParticipantId::from_uuid(session.participant_id),
        )
        .await?;
        if let MediaCredentialActor::Endpoint(identity) = &actor {
            // An endpoint only ever obtains credentials for its own sessions.
            if session.endpoint_id != Some(identity.endpoint_id().as_uuid())
                || participant.principal_id != identity.principal_id().as_uuid()
            {
                return Err(CallError::NotFound);
            }
        }
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !media_state(call.current_state) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: "ISSUE_MEDIA_CREDENTIAL",
            });
        }
        if session.removed_at.is_some()
            || session.state == SessionState::Ended
            || session.media_access != MediaAccess::Authorized
            || participant.ended_at.is_some()
        {
            return Err(CallError::InvalidState("the session holds no media access"));
        }
        self.calls
            .media_binding_in(&txn, call.id)
            .await?
            .filter(|b| b.status == MediaSessionStatus::Active)
            .ok_or(CallError::InvalidState(
                "the call's media resource is not ready",
            ))?;
        let permissions = self
            .effective_permissions(&txn, &call, participant.id)
            .await?;
        let ttl = self.credentials.ttl;
        let expires_at = self.calls.now() + ttl;
        let session = self
            .calls
            .issue_media_generation_in(&txn, &cmd, &call, session, expires_at, permissions)
            .await?;
        let credential = provider
            .issue_credentials(&MediaCredentialRequest {
                session: resource(call.id),
                identity: session.media_identity.clone(),
                generation: session.media_generation,
                ttl,
            })
            .await
            .map_err(|_| CallError::MediaUnavailable)?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            session_id = %session.id,
            media_generation = session.media_generation,
            "media credential issued"
        );
        Ok(MediaCredentialView::new(
            session.id,
            &credential,
            permissions,
        ))
    }

    /// Provider-side removal of sessions the database already revoked: strip, then
    /// disconnect; confirmed removals clear the pending flag.
    pub(crate) async fn enforce_removals(
        &self,
        call: &calls::Model,
        sessions: Vec<call_sessions::Model>,
    ) {
        let Some(provider) = self.media.as_ref() else {
            return;
        };
        for session in sessions {
            let participant = MediaParticipantRef {
                session: resource(call.id),
                identity: session.media_identity.clone(),
            };
            let stripped = provider
                .update_permissions(&participant, MediaPermissions::NONE)
                .await;
            let removed = provider.remove_session(&participant).await;
            let confirmed = stripped.is_ok() && removed.is_ok();
            let result: CallResult<()> = async {
                let txn = self.db().begin().await?;
                let current = call_sessions::Entity::find_by_id(session.id)
                    .one(&txn)
                    .await?
                    .ok_or(CallError::NotFound)?;
                if confirmed {
                    self.calls
                        .set_session_enforcement_in(&txn, current, false)
                        .await?;
                } else if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
                    let failure = removed.err().or(stripped.err()).map(|e| e.failure());
                    if let Some(failure) = failure {
                        let cmd = system_cmd(call);
                        self.calls
                            .record_media_failure_in(
                                &txn,
                                &cmd,
                                call,
                                binding,
                                "REMOVE_PARTICIPANT",
                                failure,
                            )
                            .await?;
                    }
                }
                txn.commit().await?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(call_id = %call.id, %error, "removal enforcement not recorded; reconciliation retries");
            }
        }
    }

    /// Authenticated provider callback (§45–§47). Verify, claim (dedupe), process as
    /// evidence, complete. Unsigned / forged callbacks are refused before anything else.
    pub async fn handle_provider_event(
        &self,
        authorization: Option<&str>,
        body: &[u8],
    ) -> Result<CallbackResult, EventVerificationError> {
        let Some(provider) = self.media.as_ref() else {
            return Err(EventVerificationError::InvalidSignature);
        };
        let event = provider.verify_event(authorization, body)?;
        let Some(call_id) = event
            .resource_name
            .as_deref()
            .and_then(MediaResourceName::call_id)
        else {
            // Another service's resource (e.g. a meeting room): not Calling's evidence.
            return Ok(CallbackResult::NotOurs);
        };
        let adapter = provider.adapter_name();
        let result: CallResult<CallbackResult> = async {
            // Claim: the unique key makes a duplicate delivery a no-op.
            let now = self.calls.now();
            let record_id = MediaProviderEventId::generate().as_uuid();
            let claimed = self
                .db()
                .execute(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DbBackend::Postgres,
                    "INSERT INTO call_media_provider_events \
                        (id, provider_adapter, provider_event_id, event_kind, identity_status, occurred_at, received_at) \
                     VALUES ($1, $2, $3, $4, 'NONE', $5, $6) \
                     ON CONFLICT ON CONSTRAINT uq_call_media_provider_events_dedupe DO NOTHING",
                    vec![
                        record_id.into(),
                        adapter.into(),
                        event.event_id.clone().into(),
                        event.kind.as_str().into(),
                        event.occurred_at.into(),
                        now.into(),
                    ],
                ))
                .await?
                .rows_affected();
            if claimed == 0 {
                return Ok(CallbackResult::Duplicate);
            }
            let (outcome, identity_status, session_id, call) =
                self.process_event(call_id, &event).await?;
            let mut record: call_media_provider_events::ActiveModel =
                call_media_provider_events::Entity::find_by_id(record_id)
                    .one(self.db())
                    .await?
                    .ok_or(CallError::NotFound)?
                    .into();
            if let Some(call) = &call {
                record.call_id = Set(Some(call.id));
                record.tenant_id = Set(Some(call.tenant_id));
                record.application_id = Set(Some(call.application_id));
                record.environment_id = Set(Some(call.environment_id));
                let txn = self.db().begin().await?;
                if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
                    record.media_session_id = Set(Some(binding.id));
                }
                txn.commit().await?;
            }
            record.session_id = Set(session_id);
            record.identity_status = Set(identity_status);
            record.outcome = Set(Some(outcome));
            record.processed_at = Set(Some(self.calls.now().max(now)));
            sea_orm::ActiveModelTrait::update(record, self.db()).await?;
            Ok(CallbackResult::Processed(outcome))
        }
        .await;
        result.map_err(|error| {
            tracing::error!(%error, "provider callback processing failed");
            EventVerificationError::Malformed
        })
    }

    async fn process_event(
        &self,
        call_id: CallId,
        event: &crate::media::provider::VerifiedMediaEvent,
    ) -> CallResult<(
        MediaEventOutcome,
        MediaIdentityStatus,
        Option<Uuid>,
        Option<calls::Model>,
    )> {
        let Some(call) = calls::Entity::find_by_id(call_id.as_uuid())
            .one(self.db())
            .await?
        else {
            return Ok((
                MediaEventOutcome::Quarantined,
                MediaIdentityStatus::None,
                None,
                None,
            ));
        };
        let participant = event.participant.as_ref();
        let session = match participant {
            Some(p) => {
                call_sessions::Entity::find()
                    .filter(call_sessions::Column::CallId.eq(call.id))
                    .filter(call_sessions::Column::MediaIdentity.eq(p.identity.as_str()))
                    .one(self.db())
                    .await?
            }
            None => None,
        };
        let identity_status = match (participant, &session) {
            (None, _) => MediaIdentityStatus::None,
            (Some(_), Some(_)) => MediaIdentityStatus::Known,
            (Some(_), None) => MediaIdentityStatus::Unknown,
        };
        let session_id = session.as_ref().map(|s| s.id);
        let outcome = match event.kind {
            MediaEventKind::ParticipantJoined => {
                let p = participant.ok_or(CallError::Validation("participant missing"))?;
                match self.admit(&call, p).await? {
                    Some(EvictionReason::UnknownIdentity) => MediaEventOutcome::Quarantined,
                    Some(_) if call.current_state.is_terminal() => {
                        MediaEventOutcome::IgnoredTerminal
                    }
                    Some(_) => MediaEventOutcome::Evicted,
                    None => MediaEventOutcome::Applied,
                }
            }
            MediaEventKind::ParticipantLeft | MediaEventKind::ParticipantConnectionAborted => {
                let reason = if event.kind == MediaEventKind::ParticipantLeft {
                    DisconnectReason::Left
                } else {
                    DisconnectReason::ConnectionLost
                };
                match (participant, session) {
                    (Some(p), Some(s)) => {
                        if self.record_absent(&call, s.id, &p.identity, reason).await? {
                            MediaEventOutcome::Applied
                        } else {
                            MediaEventOutcome::NoOp
                        }
                    }
                    (Some(_), None) => MediaEventOutcome::Quarantined,
                    _ => MediaEventOutcome::NoOp,
                }
            }
            MediaEventKind::ResourceStarted
                if call.current_state.is_terminal() || call.current_state == CallState::Ending =>
            {
                // A resource of a finished Call must not exist.
                self.teardown_media(call.id).await;
                MediaEventOutcome::Applied
            }
            MediaEventKind::ResourceFinished => {
                if call.current_state.is_terminal() || call.current_state == CallState::Ending {
                    self.teardown_media(call.id).await;
                    if call.current_state == CallState::Ending {
                        self.finish_ending(call.id).await?;
                    }
                    MediaEventOutcome::Applied
                } else {
                    // An active Call lost its resource: reconciliation re-ensures it.
                    self.schedule_reconcile_now(call.id).await?;
                    MediaEventOutcome::Applied
                }
            }
            _ if call.current_state.is_terminal() => MediaEventOutcome::IgnoredTerminal,
            _ => MediaEventOutcome::NoOp,
        };
        Ok((outcome, identity_status, session_id, Some(call)))
    }

    async fn schedule_reconcile_now(&self, call_id: Uuid) -> CallResult<()> {
        crate::calls::service::execute(
            self.db(),
            "UPDATE call_media_sessions SET next_reconcile_at = LEAST(next_reconcile_at, $2) \
              WHERE call_id = $1 AND status <> 'TERMINATED'",
            vec![call_id.into(), self.calls.now().into()],
        )
        .await?;
        Ok(())
    }

    /// Database facts about the session a provider identity claims to be.
    async fn facts(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        session: &call_sessions::Model,
    ) -> CallResult<SessionMediaFacts> {
        let participant_active = call_participants::Entity::find_by_id(session.participant_id)
            .one(txn)
            .await?
            .is_some_and(|p| p.ended_at.is_none());
        let authorized = session.media_access == MediaAccess::Authorized
            && session.state != SessionState::Ended
            && session.removed_at.is_none()
            && participant_active
            && media_state(call.current_state);
        Ok(SessionMediaFacts {
            authorized,
            generation: session.media_generation,
            permissions: if authorized {
                self.effective_permissions(txn, call, session.participant_id)
                    .await?
            } else {
                MediaPermissions::NONE
            },
        })
    }

    /// Admission of one provider connection. `None` = admitted (permissions applied,
    /// join recorded); `Some(reason)` = evicted.
    pub(crate) async fn admit(
        &self,
        call: &calls::Model,
        participant: &ProviderParticipant,
    ) -> CallResult<Option<EvictionReason>> {
        let provider = self.provider()?.clone();
        let txn = self.db().begin().await?;
        let session = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .filter(call_sessions::Column::MediaIdentity.eq(participant.identity.as_str()))
            .one(&txn)
            .await?;
        let call = self.locked(&txn, call).await?;
        let facts = match &session {
            Some(s) => Some(self.facts(&txn, &call, s).await?),
            None => None,
        };
        txn.commit().await?;
        let target = MediaParticipantRef {
            session: resource(call.id),
            identity: participant.identity.clone(),
        };
        match admission(facts, participant.generation) {
            Admission::Evict(reason) => {
                let _ = provider
                    .update_permissions(&target, MediaPermissions::NONE)
                    .await;
                let removed = provider.remove_session(&target).await;
                let txn = self.db().begin().await?;
                let call = self.locked(&txn, &call).await?;
                let cmd = system_cmd(&call);
                let mut details = vec![
                    ("reason", SafeValue::Text(reason.as_str().into())),
                    ("confirmed", SafeValue::Bool(removed.is_ok())),
                ];
                if let Some(s) = &session {
                    details.push(("session_id", SafeValue::Text(s.id.to_string())));
                }
                self.calls
                    .record_media_fact_in(
                        &txn,
                        &cmd,
                        &call,
                        CallEventType::CallMediaParticipantEvicted,
                        details,
                    )
                    .await?;
                txn.commit().await?;
                tracing::info!(call_id = %call.id, reason = reason.as_str(), "media connection evicted");
                Ok(Some(reason))
            }
            Admission::Admit(permissions) => {
                provider
                    .update_permissions(&target, permissions)
                    .await
                    .map_err(|_| CallError::MediaUnavailable)?;
                let session = session.expect("admitted sessions exist");
                if participant.active {
                    self.record_joined(&call, session.id).await?;
                }
                Ok(None)
            }
        }
    }

    /// Records a verified, admitted connection: opens (or re-opens) the session's
    /// connection interval, marks it admitted, then advances the Call (CONNECTED, transfer
    /// completion) on positive evidence.
    pub(crate) async fn record_joined(
        &self,
        call: &calls::Model,
        session_id: Uuid,
    ) -> CallResult<()> {
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(call), CallId::from_uuid(call.id))
            .await?;
        if call.current_state.is_terminal() || call.current_state == CallState::Ending {
            txn.commit().await?;
            return Ok(());
        }
        let session =
            crate::calls::service::session_of(&txn, &call, CallSessionId::from_uuid(session_id))
                .await?;
        if session.media_access != MediaAccess::Authorized || session.state == SessionState::Ended {
            txn.commit().await?;
            return Ok(());
        }
        let cmd = system_cmd(&call);
        let newly = matches!(
            session.state,
            SessionState::Created | SessionState::Disconnected
        );
        let session = match session.state {
            SessionState::Created => {
                self.calls
                    .open_interval_in(&txn, &cmd, &call, session, SessionState::Created)
                    .await?
                    .0
            }
            SessionState::Disconnected => {
                self.calls
                    .open_interval_in(&txn, &cmd, &call, session, SessionState::Disconnected)
                    .await?
                    .0
            }
            _ => session,
        };
        if newly || session.media_admitted_at.is_none() {
            let session = self.calls.admit_session_media_in(&txn, session).await?;
            let permissions = self
                .effective_permissions(&txn, &call, session.participant_id)
                .await?;
            self.calls
                .record_media_fact_in(
                    &txn,
                    &cmd,
                    &call,
                    CallEventType::CallMediaPermissionsUpdated,
                    vec![
                        ("reason", SafeValue::Text("ADMISSION".into())),
                        ("session_id", SafeValue::Text(session.id.to_string())),
                        (
                            "can_publish_audio",
                            SafeValue::Bool(permissions.can_publish_audio),
                        ),
                        (
                            "can_publish_video",
                            SafeValue::Bool(permissions.can_publish_video),
                        ),
                        ("can_subscribe", SafeValue::Bool(permissions.can_subscribe)),
                    ],
                )
                .await?;
        }
        txn.commit().await?;
        self.try_confirm_connected(call.id).await?;
        self.complete_transfer_if_joined(call.id).await?;
        Ok(())
    }

    /// A connection left: the session's interval closes — unless the provider still lists
    /// the identity (a replacement connection of the same session raced the callback).
    pub(crate) async fn record_absent(
        &self,
        call: &calls::Model,
        session_id: Uuid,
        identity: &str,
        reason: DisconnectReason,
    ) -> CallResult<bool> {
        if let Some(provider) = self.media.as_ref() {
            if let Ok(present) = provider.list_participants(&resource(call.id)).await {
                if present.iter().any(|p| p.identity == identity) {
                    return Ok(false);
                }
            }
        }
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(call), CallId::from_uuid(call.id))
            .await?;
        let session =
            crate::calls::service::session_of(&txn, &call, CallSessionId::from_uuid(session_id))
                .await?;
        if session.state != SessionState::Connected {
            txn.commit().await?;
            return Ok(false);
        }
        let cmd = system_cmd(&call);
        self.calls
            .disconnect_session_in(&txn, &cmd, &call, session, reason)
            .await?;
        txn.commit().await?;
        Ok(true)
    }

    /// ANSWERED → CONNECTED on positive evidence (§50): admitted, connected sessions of two
    /// different participants that the provider currently confirms. A token issued, a
    /// resource existing or a single join is never enough.
    pub(crate) async fn try_confirm_connected(&self, call_id: Uuid) -> CallResult<bool> {
        let call = calls::Entity::find_by_id(call_id)
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)?;
        if call.current_state != CallState::Answered {
            return Ok(false);
        }
        let evidence = {
            let txn = self.db().begin().await?;
            let evidence = self.media_path_evidence(&txn, &call, None).await?;
            txn.commit().await?;
            evidence
        };
        let Some(evidence) = evidence else {
            return Ok(false);
        };
        if !self.provider_confirms(&call, &evidence).await {
            return Ok(false);
        }
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(&call), CallId::from_uuid(call.id))
            .await?;
        if call.current_state != CallState::Answered {
            txn.commit().await?;
            return Ok(false);
        }
        let cmd = system_cmd(&call);
        let call = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                call.version,
                CallAction::ConfirmConnected { evidence },
            )
            .await?
            .call;
        if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
            let leg = leg_of(&txn, &call, CallLegId::from_uuid(binding.leg_id)).await?;
            if leg.state.can_transition_to(LegState::Connected) {
                self.calls
                    .move_leg_in(&txn, &cmd, &call, leg, LegState::Connected, None)
                    .await?;
            }
        }
        let mut pending = PendingSignals::default();
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Connected,
            &call,
            cmd.correlation_id().as_str(),
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        tracing::info!(call_id = %call.id, "call connected on verified media evidence");
        Ok(true)
    }

    /// Reconciles one Call's media (§48) and returns the number of repairs.
    pub async fn reconcile_call(&self, call_id: Uuid) -> CallResult<usize> {
        let provider = self.provider()?.clone();
        let call = calls::Entity::find_by_id(call_id)
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)?;
        let binding = {
            let txn = self.db().begin().await?;
            let b = self.calls.media_binding_in(&txn, call.id).await?;
            txn.commit().await?;
            b
        };
        let Some(binding) = binding else {
            return Ok(0);
        };
        // Creation interrupted before ringing: resume it.
        if binding.status == MediaSessionStatus::Ensuring
            && call.current_state == CallState::Routing
        {
            self.advance_routing(call.id).await?;
            return Ok(1);
        }
        // Ending / terminal Calls: tear down and confirm.
        if call.current_state == CallState::Ending {
            return Ok(usize::from(self.finish_ending(call.id).await?));
        }
        if call.current_state.is_terminal() || binding.status == MediaSessionStatus::Terminating {
            // A terminal Call owns no resource: terminate whatever the provider still has.
            let lingering = provider
                .reconcile(&resource(call.id))
                .await
                .map(|s| s.exists)
                .unwrap_or(true);
            if lingering || binding.status != MediaSessionStatus::Terminated {
                self.teardown_media(call.id).await;
                return Ok(1);
            }
            return Ok(0);
        }
        let snapshot = provider
            .reconcile(&resource(call.id))
            .await
            .map_err(|_| CallError::MediaUnavailable)?;
        let mut repairs = 0usize;
        if !snapshot.exists {
            // The Call still needs its resource: re-ensure it (same canonical name).
            provider
                .create_or_ensure_session(
                    &resource(call.id),
                    &crate::media::provider::MediaSessionSpec {
                        max_participants: binding.max_participants as u32,
                        empty_timeout: self.config.media_empty_timeout,
                    },
                )
                .await
                .map_err(|_| CallError::MediaUnavailable)?;
            repairs += 1;
        }
        let expected = {
            let txn = self.db().begin().await?;
            let call = self.locked(&txn, &call).await?;
            let mut expected = Vec::new();
            for session in call_sessions::Entity::find()
                .filter(call_sessions::Column::CallId.eq(call.id))
                .all(&txn)
                .await?
            {
                expected.push(ExpectedSession {
                    session_id: session.id,
                    identity: session.media_identity.clone(),
                    facts: self.facts(&txn, &call, &session).await?,
                    connected: session.state == SessionState::Connected,
                });
            }
            txn.commit().await?;
            expected
        };
        for action in plan(&expected, &snapshot.participants) {
            repairs += 1;
            match action {
                RepairAction::Evict { identity, .. } => {
                    if let Some(p) = snapshot
                        .participants
                        .iter()
                        .find(|p| p.identity == identity)
                    {
                        self.admit(&call, p).await?;
                    }
                }
                RepairAction::ApplyPermissions { identity, .. } => {
                    if let Some(p) = snapshot
                        .participants
                        .iter()
                        .find(|p| p.identity == identity)
                    {
                        self.admit(&call, p).await?;
                    }
                }
                RepairAction::RecordJoined { session_id } => {
                    let identity = expected
                        .iter()
                        .find(|e| e.session_id == session_id)
                        .map(|e| e.identity.clone())
                        .unwrap_or_default();
                    if let Some(p) = snapshot
                        .participants
                        .iter()
                        .find(|p| p.identity == identity)
                    {
                        self.admit(&call, p).await?;
                    }
                }
                RepairAction::RecordAbsent { session_id } => {
                    let identity = expected
                        .iter()
                        .find(|e| e.session_id == session_id)
                        .map(|e| e.identity.clone())
                        .unwrap_or_default();
                    self.record_absent(
                        &call,
                        session_id,
                        &identity,
                        DisconnectReason::ConnectionLost,
                    )
                    .await?;
                }
            }
        }
        // Removed sessions whose connection is gone (after this round's evictions, as the
        // provider now lists it): enforcement confirmed. Unknown listing: retry later.
        let remaining = provider.list_participants(&resource(call.id)).await.ok();
        let txn = self.db().begin().await?;
        let call = self.locked(&txn, &call).await?;
        if let Some(remaining) = remaining {
            for session in call_sessions::Entity::find()
                .filter(call_sessions::Column::CallId.eq(call.id))
                .filter(call_sessions::Column::MediaEnforcementPending.eq(true))
                .all(&txn)
                .await?
            {
                if !remaining
                    .iter()
                    .any(|p| p.identity == session.media_identity)
                {
                    self.calls
                        .set_session_enforcement_in(&txn, session, false)
                        .await?;
                }
            }
        }
        let cmd = system_cmd(&call);
        if repairs > 0 {
            self.calls
                .record_media_fact_in(
                    &txn,
                    &cmd,
                    &call,
                    CallEventType::CallMediaReconciled,
                    vec![("repairs", SafeValue::from(repairs))],
                )
                .await?;
        }
        if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
            let next = self.calls.now() + self.config.reconcile_interval;
            self.calls
                .mark_media_reconciled_in(&txn, binding, true, next)
                .await?;
        }
        txn.commit().await?;
        self.try_confirm_connected(call.id).await?;
        self.complete_transfer_if_joined(call.id).await?;
        Ok(repairs)
    }
}

/// The media connection gateway's authority (ADR 0007): every fresh signaling connection
/// and reconnection is decided here, from the database, before the media server is
/// contacted. No cache: a revocation committed before the check is always seen. Any error
/// reading the state is a refusal (fail closed).
#[async_trait::async_trait]
impl ConnectionAdmission for AppCallService {
    async fn admit_connection(&self, claims: &ConnectionClaims) -> Result<(), ConnectionDenied> {
        let decision = self.connection_decision(claims).await;
        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                tracing::warn!(%error, "media connection refused: authoritative state unavailable");
                Err(ConnectionDenied::Unavailable)
            }
        };
        if let Err(denied) = decision {
            tracing::info!(
                reason = denied.as_str(),
                resource = %claims.resource_name,
                "media connection refused before admission to the media server"
            );
        }
        decision
    }

    async fn connection_established(&self, claims: &ConnectionClaims) {
        let Some(call_id) = MediaResourceName::call_id(&claims.resource_name) else {
            return;
        };
        let result: CallResult<()> = async {
            let Some(call) = calls::Entity::find_by_id(call_id.as_uuid())
                .one(self.db())
                .await?
            else {
                return Ok(());
            };
            let provider = self.provider()?.clone();
            let participant = provider
                .list_participants(&resource(call.id))
                .await
                .map_err(|_| CallError::MediaUnavailable)?
                .into_iter()
                .find(|p| p.identity == claims.identity);
            if let Some(participant) = participant {
                self.admit(&call, &participant).await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "post-connection admission deferred to callbacks / reconciliation");
        }
    }
}

impl AppCallService {
    /// The admission decision for one claimed session generation (see
    /// [`ConnectionAdmission`]). `Err(CallError)`: state unreadable (the caller refuses).
    async fn connection_decision(
        &self,
        claims: &ConnectionClaims,
    ) -> CallResult<Result<(), ConnectionDenied>> {
        let Some(call_id) = MediaResourceName::call_id(&claims.resource_name) else {
            return Ok(Err(ConnectionDenied::ForeignResource));
        };
        let txn = self.db().begin().await?;
        let Some(session) = call_sessions::Entity::find()
            .filter(call_sessions::Column::MediaIdentity.eq(claims.identity.as_str()))
            .one(&txn)
            .await?
        else {
            txn.commit().await?;
            return Ok(Err(ConnectionDenied::UnknownIdentity));
        };
        if session.call_id != call_id.as_uuid() {
            txn.commit().await?;
            return Ok(Err(ConnectionDenied::ForeignResource));
        }
        let call = calls::Entity::find_by_id(session.call_id)
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        // Ownership is consistent by construction (composite keys); checked anyway.
        if scope_of(&call)
            != CallScope::from_parts(
                crate::domain::TenantId::from_uuid(session.tenant_id),
                crate::domain::ApplicationId::from_uuid(session.application_id),
                crate::domain::EnvironmentId::from_uuid(session.environment_id),
            )
        {
            txn.commit().await?;
            return Ok(Err(ConnectionDenied::ForeignResource));
        }
        let binding_active = self
            .calls
            .media_binding_in(&txn, call.id)
            .await?
            .is_some_and(|b| b.status == MediaSessionStatus::Active);
        let facts = self.facts(&txn, &call, &session).await?;
        txn.commit().await?;
        if !binding_active {
            return Ok(Err(ConnectionDenied::SessionRevoked));
        }
        Ok(match admission(Some(facts), claims.generation) {
            Admission::Admit(_) => Ok(()),
            Admission::Evict(EvictionReason::StaleGeneration) => {
                Err(ConnectionDenied::StaleGeneration)
            }
            Admission::Evict(EvictionReason::UnknownIdentity) => {
                Err(ConnectionDenied::UnknownIdentity)
            }
            Admission::Evict(EvictionReason::SessionRevoked) => {
                Err(ConnectionDenied::SessionRevoked)
            }
        })
    }
}
