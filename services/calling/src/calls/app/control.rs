//! Call control (Part C §55–§63, §69, §90): cancel, end, hold, resume, individual session
//! removal and the available-actions view. Every command is scope-confined, version-
//! protected (`expectedVersion`), idempotent with an Idempotency-Key, audited by
//! immutable events with outbox rows, and enforced at the media provider where media is
//! involved — the database state never claims an enforcement the provider did not
//! confirm.

use std::collections::HashSet;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};
use serde::Serialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    ringing::replay, scope_of, summary, system_cmd, AppCallService, CommandOutcome, PendingSignals,
};
use crate::{
    calls::{
        attributes::SafeValue,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallId, CallSessionId},
        kinds::{
            AttemptOutcome, CommandOperation, DisconnectReason, InvitationStatus,
            InvitationTerminalReason, MediaAccess, MediaSessionStatus, SessionState,
            TransferFailureReason,
        },
        quality::{NewQualityRecord, QualityMetrics},
        service::{session_of, CommandKey},
        state::CallState,
        transitions::{CallAction, EndedEvidence},
    },
    media::{
        permissions::media_state,
        provider::{MediaParticipantRef, MediaResourceName, MediaSessionRef},
        MediaPermissions,
    },
    models::{call_attempts, call_sessions, calls},
    platform::{context::PlatformAuthContext, scopes::names},
    signaling::signals::SignalType,
};

/// One action of the available-actions view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionAvailability {
    pub action: &'static str,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

/// Server-derived actions currently valid for a Call (§69). No provider name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableActions {
    pub call_id: Uuid,
    pub call_state: &'static str,
    pub call_version: i64,
    pub actions: Vec<ActionAvailability>,
}

fn control_body(call: &calls::Model, operation: CommandOperation) -> Value {
    json!({ "call": summary(call), "operation": operation.as_str() })
}

impl AppCallService {
    /// CANCEL (§60): pre-active termination (REQUESTED … ANSWERED). Cancels live
    /// invitations and outstanding attempts, closes route / leg / sessions, tears the
    /// media resource down. Repeating it on a CANCELLED Call is a safe no-op.
    pub async fn cancel(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, cmd.scope(), key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, cmd.scope(), call_id).await?;
        if call.current_state == CallState::Cancelled {
            let body = control_body(&call, CommandOperation::Cancel);
            txn.commit().await?;
            return Ok(CommandOutcome {
                body,
                replayed: true,
            });
        }
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        let applied = self
            .calls
            .transition_in(&txn, cmd, &call, expected_version, CallAction::Cancel)
            .await?;
        let call = applied.call;
        // An enterprise Call cancelled before it was answered: abandoned (queue entry
        // ABANDONED, business outcome recorded).
        let queue = self.enterprise_call_ended_in(&txn, &call).await?;
        let body = control_body(&call, CommandOperation::Cancel);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    None,
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        for invitation in &applied.cancelled_invitations {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::RingCancelled,
                &call,
                invitation,
                &correlation,
            )
            .await?;
        }
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Cancelled,
            &call,
            &correlation,
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        if let Some(queue) = queue {
            self.queue_position_signals(queue).await;
        }
        // Provider teardown after commit; failures are retried by reconciliation.
        self.teardown_media(call.id).await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// END (§61): an answered / connected / held / transferring Call goes ENDING; live
    /// offers, the in-flight transfer and pending attempts stop; provider cleanup runs;
    /// ENDING → ENDED only with cleanup evidence. If cleanup fails the Call stays ENDING
    /// (truthfully) and reconciliation retries.
    pub async fn end(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, cmd.scope(), key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, cmd.scope(), call_id).await?;
        if matches!(call.current_state, CallState::Ending | CallState::Ended) {
            let body = control_body(&call, CommandOperation::End);
            txn.commit().await?;
            return Ok(CommandOutcome {
                body,
                replayed: true,
            });
        }
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        let call = self
            .calls
            .transition_in(&txn, cmd, &call, expected_version, CallAction::RequestEnd)
            .await?
            .call;
        let cancelled = self
            .calls
            .finish_live_invitations_in(
                &txn,
                cmd,
                &call,
                None,
                None,
                InvitationStatus::Cancelled,
                InvitationTerminalReason::CallTerminated,
            )
            .await?;
        if let Some(transfer) = self.calls.active_transfer(&txn, &call).await? {
            self.calls
                .cancel_transfer_in(&txn, cmd, &call, transfer, TransferFailureReason::CallEnded)
                .await?;
        }
        for attempt in call_attempts::Entity::find()
            .filter(call_attempts::Column::CallId.eq(call.id))
            .filter(call_attempts::Column::Outcome.eq(AttemptOutcome::Pending))
            .all(&txn)
            .await?
        {
            self.calls
                .finish_attempt_in(&txn, cmd, &call, attempt, AttemptOutcome::Abandoned, None)
                .await?;
        }
        let body = control_body(&call, CommandOperation::End);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    None,
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
        let correlation = cmd.correlation_id().as_str().to_string();
        for invitation in &cancelled {
            self.invitation_signal(
                &txn,
                &mut pending,
                SignalType::RingCancelled,
                &call,
                invitation,
                &correlation,
            )
            .await?;
        }
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Ended,
            &call,
            &correlation,
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        if let Err(error) = self.finish_ending(call.id).await {
            tracing::warn!(call_id = %call.id, %error, "call cleanup pending; reconciliation will retry");
        }
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Provider cleanup of an ENDING Call and, with its evidence, ENDING → ENDED with
    /// usage and quality finalized. Returns whether the Call ended.
    pub(crate) async fn finish_ending(&self, call_id: Uuid) -> CallResult<bool> {
        let call = calls::Entity::find_by_id(call_id)
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)?;
        if call.current_state != CallState::Ending {
            return Ok(call.current_state == CallState::Ended);
        }
        // Public legs end first (Telephony confirms with LEG_ENDED, which finishes the
        // Call); the Call never reports ENDED while a paid leg may still be live.
        if self.release_public_legs(call.id).await? > 0 || self.live_public_legs(call.id).await? > 0
        {
            return Ok(false);
        }
        let failure = self.cleanup_media(&call).await?;
        let txn = self.db().begin().await?;
        let call = self
            .calls
            .lock_call(&txn, scope_of(&call), CallId::from_uuid(call.id))
            .await?;
        if call.current_state != CallState::Ending {
            txn.commit().await?;
            return Ok(call.current_state == CallState::Ended);
        }
        let cmd = system_cmd(&call);
        if let Some(failure) = failure {
            if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
                self.calls
                    .record_media_failure_in(
                        &txn,
                        &cmd,
                        &call,
                        binding,
                        "TERMINATE_RESOURCE",
                        failure,
                    )
                    .await?;
            }
            txn.commit().await?;
            return Ok(false);
        }
        let ended = self
            .calls
            .transition_in(
                &txn,
                &cmd,
                &call,
                call.version,
                CallAction::ConfirmEnded {
                    evidence: EndedEvidence::CleanupConfirmed,
                },
            )
            .await?
            .call;
        self.after_media_teardown_in(&txn, &cmd, &ended).await?;
        self.calls.finalize_app_usage_in(&txn, &cmd, &ended).await?;
        self.record_quality_summary_in(&txn, &cmd, &ended).await?;
        let mut pending = PendingSignals::default();
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::Ended,
            &ended,
            cmd.correlation_id().as_str(),
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        tracing::info!(call_id = %ended.id, "call ended after confirmed media cleanup");
        Ok(true)
    }

    /// Marks the media binding TERMINATED and clears session enforcement flags once the
    /// provider confirmed the teardown.
    pub(crate) async fn after_media_teardown_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
    ) -> CallResult<()> {
        if let Some(binding) = self.calls.media_binding_in(txn, call.id).await? {
            if binding.status != MediaSessionStatus::Terminated {
                self.calls
                    .terminate_media_binding_in(txn, cmd, call, binding, false)
                    .await?;
            }
        }
        for session in call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .filter(call_sessions::Column::MediaEnforcementPending.eq(true))
            .all(txn)
            .await?
        {
            self.calls
                .set_session_enforcement_in(txn, session, false)
                .await?;
        }
        Ok(())
    }

    /// Removes every media connection of the Call and terminates its resource. `None` on
    /// confirmed cleanup (including "already absent"), else the neutral failure.
    pub(crate) async fn cleanup_media(
        &self,
        call: &calls::Model,
    ) -> CallResult<Option<crate::calls::kinds::MediaFailure>> {
        let Some(provider) = self.media.as_ref() else {
            return Ok(None);
        };
        let binding = {
            let txn = self.db().begin().await?;
            let binding = self.calls.media_binding_in(&txn, call.id).await?;
            txn.commit().await?;
            binding
        };
        if binding.is_none() {
            return Ok(None);
        }
        let session = MediaSessionRef {
            name: MediaResourceName::for_call(CallId::from_uuid(call.id)),
        };
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .all(self.db())
            .await?;
        for s in sessions
            .iter()
            .filter(|s| s.media_access == MediaAccess::Authorized || s.media_enforcement_pending)
        {
            let participant = MediaParticipantRef {
                session: session.clone(),
                identity: s.media_identity.clone(),
            };
            // Strip first (the provider hands the client a grant-less token), then remove.
            if let Err(error) = provider
                .update_permissions(&participant, MediaPermissions::NONE)
                .await
            {
                return Ok(Some(error.failure()));
            }
            if let Err(error) = provider.remove_session(&participant).await {
                return Ok(Some(error.failure()));
            }
        }
        match provider.terminate_session(&session).await {
            Ok(_) => Ok(None),
            Err(error) => Ok(Some(error.failure())),
        }
    }

    /// After a terminal transition: provider teardown, then the binding is TERMINATED
    /// with evidence (or retried by reconciliation).
    pub(crate) async fn teardown_media(&self, call_id: Uuid) {
        if let Err(error) = self.release_public_legs(call_id).await {
            tracing::warn!(%call_id, %error, "public leg release pending; reconciliation retries");
        }
        let result: CallResult<()> = async {
            let call = calls::Entity::find_by_id(call_id)
                .one(self.db())
                .await?
                .ok_or(CallError::NotFound)?;
            let failure = self.cleanup_media(&call).await?;
            let txn = self.db().begin().await?;
            let call = calls::Entity::find_by_id(call_id)
                .lock_exclusive()
                .one(&txn)
                .await?
                .ok_or(CallError::NotFound)?;
            let cmd = system_cmd(&call);
            match failure {
                None => self.after_media_teardown_in(&txn, &cmd, &call).await?,
                Some(failure) => {
                    if let Some(binding) = self.calls.media_binding_in(&txn, call.id).await? {
                        self.calls
                            .record_media_failure_in(
                                &txn,
                                &cmd,
                                &call,
                                binding,
                                "TERMINATE_RESOURCE",
                                failure,
                            )
                            .await?;
                    }
                }
            }
            if call.current_state.is_terminal() && call.terminal_reason.is_some() {
                // Finalize usage of terminal Calls once (idempotent keys).
                self.calls.finalize_app_usage_in(&txn, &cmd, &call).await?;
            }
            txn.commit().await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%call_id, %error, "media teardown pending; reconciliation will retry");
        }
    }

    /// Server-observed, bounded quality summary of an ended Call. Provider transport
    /// metrics are recorded only when the provider exposes them (never invented).
    async fn record_quality_summary_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
    ) -> CallResult<()> {
        let (Some(connected_at), Some(ended_at)) = (call.connected_at, call.ended_at) else {
            return Ok(());
        };
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .all(txn)
            .await?;
        let admitted = sessions
            .iter()
            .filter(|s| s.media_admitted_at.is_some())
            .count();
        let reconnects: i32 = sessions.iter().map(|s| (s.generation - 1).max(0)).sum();
        let participants: HashSet<Uuid> = sessions
            .iter()
            .filter(|s| s.first_connected_at.is_some())
            .map(|s| s.participant_id)
            .collect();
        let metrics = QualityMetrics::new()
            .with("media_sessions_admitted", admitted as f64)?
            .with("session_reconnects", reconnects as f64)?
            .with("connected_participants", participants.len() as f64)?;
        self.calls
            .record_quality_in(
                txn,
                cmd,
                call,
                NewQualityRecord {
                    session_id: None,
                    leg_id: None,
                    observed_from: connected_at,
                    observed_to: ended_at.max(connected_at),
                    metrics,
                },
            )
            .await?;
        Ok(())
    }

    /// HOLD (§62): the provider first stops every audio / video publication of the Call
    /// (subscriptions and membership preserved); only then is the Call HELD.
    pub async fn hold(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        self.media_state_command(cmd, call_id, expected_version, key, CommandOperation::Hold)
            .await
    }

    /// RESUME (§63): the provider restores the re-derived, currently authorized
    /// permissions (never broader, never self-granted), then positive media-path evidence
    /// is required for HELD → CONNECTED.
    pub async fn resume(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        self.media_state_command(
            cmd,
            call_id,
            expected_version,
            key,
            CommandOperation::Resume,
        )
        .await
    }

    async fn media_state_command(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        expected_version: i64,
        key: Option<&CommandKey>,
        operation: CommandOperation,
    ) -> CallResult<CommandOutcome> {
        let scope = cmd.scope();
        if let Some(key) = key {
            if let Some(prior) = self.calls.prior_command(self.db(), scope, key).await? {
                return replay(prior);
            }
        }
        let provider = self.provider()?.clone();
        if !provider.capabilities().permission_update {
            return Err(CallError::CapabilityUnavailable(
                "hold and resume need server-side media permission updates",
            ));
        }
        let (from, to) = match operation {
            CommandOperation::Hold => (CallState::Connected, CallState::Held),
            _ => (CallState::Held, CallState::Connected),
        };
        let call = self.calls_row(scope, call_id).await?;
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        if call.current_state != from {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: operation.as_str(),
            });
        }
        // Provider enforcement first.
        let applied = self.apply_effective_permissions(&call, to).await;
        let applied = match applied {
            Ok(n) => n,
            Err(failure) => {
                // Restore what the database still says, best effort; reconciliation repairs.
                let _ = self.apply_effective_permissions(&call, from).await;
                tracing::warn!(call_id = %call.id, %failure, operation = operation.as_str(), "media enforcement failed");
                return Err(CallError::MediaUnavailable);
            }
        };
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let locked = self.calls.lock_call(&txn, scope, call_id).await?;
        if locked.version != expected_version || locked.current_state != from {
            txn.rollback().await?;
            let _ = self
                .apply_effective_permissions(&locked, locked.current_state)
                .await;
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: locked.version,
            });
        }
        let action = match operation {
            CommandOperation::Hold => CallAction::Hold,
            _ => {
                let Some(evidence) = self.media_path_evidence(&txn, &locked, None).await? else {
                    txn.rollback().await?;
                    let _ = self
                        .apply_effective_permissions(&locked, CallState::Held)
                        .await;
                    return Err(CallError::InsufficientEvidence(
                        "resume needs connected sessions of two participants",
                    ));
                };
                if !self.provider_confirms(&locked, &evidence).await {
                    txn.rollback().await?;
                    let _ = self
                        .apply_effective_permissions(&locked, CallState::Held)
                        .await;
                    return Err(CallError::InsufficientEvidence(
                        "the media provider does not confirm the media path",
                    ));
                }
                CallAction::Resume { evidence }
            }
        };
        let call = self
            .calls
            .transition_in(&txn, cmd, &locked, expected_version, action)
            .await?
            .call;
        self.calls
            .record_media_fact_in(
                &txn,
                cmd,
                &call,
                CallEventType::CallMediaPermissionsUpdated,
                vec![
                    ("reason", SafeValue::Text(operation.as_str().into())),
                    ("sessions_applied", SafeValue::from(applied)),
                ],
            )
            .await?;
        let body = control_body(&call, operation);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    None,
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
        let kind = if operation == CommandOperation::Hold {
            SignalType::Held
        } else {
            SignalType::Resumed
        };
        self.call_signal(
            &txn,
            &mut pending,
            kind,
            &call,
            cmd.correlation_id().as_str(),
            Map::new(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Applies the permissions effective in `state` to every authorized session of the
    /// Call at the provider. Returns how many connections received them.
    pub(crate) async fn apply_effective_permissions(
        &self,
        call: &calls::Model,
        state: CallState,
    ) -> Result<usize, crate::media::MediaProviderError> {
        let Some(provider) = self.media.as_ref() else {
            return Err(crate::media::MediaProviderError::Unsupported);
        };
        let txn = self
            .db()
            .begin()
            .await
            .map_err(|_| crate::media::MediaProviderError::Unavailable)?;
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .filter(call_sessions::Column::MediaAccess.eq(MediaAccess::Authorized))
            .filter(call_sessions::Column::State.ne(SessionState::Ended))
            .all(&txn)
            .await
            .map_err(|_| crate::media::MediaProviderError::Unavailable)?;
        let mut targets = Vec::new();
        for session in sessions {
            let authorized = self
                .calls
                .authorized_permissions_in(&txn, call, session.participant_id)
                .await
                .map_err(|_| crate::media::MediaProviderError::Unavailable)?;
            targets.push((
                session.media_identity.clone(),
                MediaPermissions::effective(authorized, call.call_kind, state, false),
            ));
        }
        txn.commit()
            .await
            .map_err(|_| crate::media::MediaProviderError::Unavailable)?;
        let resource = MediaSessionRef {
            name: MediaResourceName::for_call(CallId::from_uuid(call.id)),
        };
        let mut applied = 0;
        for (identity, permissions) in targets {
            match provider
                .update_permissions(
                    &MediaParticipantRef {
                        session: resource.clone(),
                        identity,
                    },
                    permissions,
                )
                .await?
            {
                crate::media::provider::PermissionOutcome::Applied => applied += 1,
                crate::media::provider::PermissionOutcome::NotPresent => {}
            }
        }
        Ok(applied)
    }

    /// Provider listing confirms every evidence session's connection is present.
    pub(crate) async fn provider_confirms(
        &self,
        call: &calls::Model,
        evidence: &crate::calls::transitions::ConnectedEvidence,
    ) -> bool {
        let Some(provider) = self.media.as_ref() else {
            return false;
        };
        if !provider.capabilities().participant_listing {
            return true;
        }
        let Ok(participants) = provider
            .list_participants(&MediaSessionRef {
                name: MediaResourceName::for_call(CallId::from_uuid(call.id)),
            })
            .await
        else {
            return false;
        };
        let Ok(sessions) = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .all(self.db())
            .await
        else {
            return false;
        };
        evidence.session_ids().iter().all(|id| {
            sessions
                .iter()
                .find(|s| s.id == id.as_uuid())
                .is_some_and(|s| {
                    participants.iter().any(|p| {
                        p.identity == s.media_identity
                            && p.active
                            && p.generation == Some(s.media_generation)
                    })
                })
        })
    }

    pub(crate) async fn calls_row(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<calls::Model> {
        calls::Entity::find_by_id(call_id.as_uuid())
            .filter(calls::Column::TenantId.eq(scope.tenant_id().as_uuid()))
            .filter(calls::Column::ApplicationId.eq(scope.application_id().as_uuid()))
            .filter(calls::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
            .one(self.db())
            .await?
            .ok_or(CallError::NotFound)
    }

    /// Removes one session (§35): DB first (ENDED, media access REVOKED, removal final,
    /// enforcement pending), then the provider strips and disconnects the connection.
    /// A provider failure never restores access in the database; reconciliation retries.
    pub async fn remove_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
        expected_version: i64,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, cmd.scope(), key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, cmd.scope(), call_id).await?;
        let session = session_of(&txn, &call, session_id).await?;
        let body = |call: &calls::Model| json!({ "call": summary(call), "operation": "REMOVE_SESSION", "sessionId": session_id, "removed": true });
        // Removing a removed session again is a safe no-op.
        if session.removed_at.is_some() {
            let body = body(&call);
            txn.commit().await?;
            return Ok(CommandOutcome {
                body,
                replayed: false,
            });
        }
        if call.version != expected_version {
            return Err(CallError::StaleVersion {
                expected: expected_version,
                current: call.version,
            });
        }
        if session.state == SessionState::Ended {
            return Err(CallError::InvalidState("the session has ended"));
        }
        let removed = self
            .calls
            .finish_session(
                &txn,
                cmd,
                &call,
                session,
                DisconnectReason::SessionEnded,
                true,
            )
            .await?;
        let body = body(&call);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    Some(removed.id),
                )
                .await?;
        }
        txn.commit().await?;
        self.enforce_removals(&call, vec![removed]).await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// Available actions of a Call for the authenticated credential (§69).
    pub async fn available_actions(
        &self,
        ctx: &PlatformAuthContext,
        call_id: CallId,
    ) -> CallResult<AvailableActions> {
        let scope = CallScope::of_platform(ctx);
        let call = self.calls_row(scope, call_id).await?;
        let can_write = ctx.has_scope(names::CALLS_WRITE);
        let capabilities = self.capabilities();
        let permission_update = capabilities.is_some_and(|c| c.permission_update);
        let app_media = capabilities.is_some_and(|c| c.supports_app_calls());
        let live_invitations = crate::models::call_invitations::Entity::find()
            .filter(crate::models::call_invitations::Column::CallId.eq(call.id))
            .all(self.db())
            .await?
            .into_iter()
            .any(|i| i.status == InvitationStatus::Ringing && i.expires_at > self.calls.now());
        let transfer_active = {
            let txn = self.db().begin().await?;
            let active = self.calls.active_transfer(&txn, &call).await?.is_some();
            txn.commit().await?;
            active
        };
        let state = call.current_state;
        let entry =
            |action: &'static str, state_ok: bool, capability_ok: bool| ActionAvailability {
                action,
                available: can_write && state_ok && capability_ok,
                reason: if !can_write {
                    Some("INSUFFICIENT_SCOPE")
                } else if !state_ok {
                    Some("STATE")
                } else if !capability_ok {
                    Some("CAPABILITY_UNAVAILABLE")
                } else {
                    None
                },
            };
        let actions = vec![
            entry("ACCEPT", live_invitations, true),
            entry("DECLINE", live_invitations, true),
            entry(
                "CANCEL",
                CallAction::Cancel.sources().contains(&state),
                true,
            ),
            entry(
                "END",
                CallAction::RequestEnd.sources().contains(&state),
                true,
            ),
            entry("HOLD", state == CallState::Connected, permission_update),
            entry("RESUME", state == CallState::Held, permission_update),
            entry(
                "TRANSFER",
                matches!(state, CallState::Connected | CallState::Held) && !transfer_active,
                app_media,
            ),
            entry("ISSUE_MEDIA_CREDENTIAL", media_state(state), app_media),
        ];
        Ok(AvailableActions {
            call_id: call.id,
            call_state: state.as_str(),
            call_version: call.version,
            actions,
        })
    }
}
