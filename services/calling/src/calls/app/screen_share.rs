//! C6 — screen sharing (`POST /v1/calls/{id}/screen-share`, `…/screen-share/stop`).
//!
//! * Policy: `CALLING_SCREEN_SHARE_ENABLED` (default ON) — any active participant of a
//!   live application Call (VOICE or VIDEO) may present. Off refuses NEW presentations
//!   (422 CAPABILITY_UNAVAILABLE); stopping always works.
//! * Presenter state is server-side and single: at most one presenter per Call (Call row
//!   lock + a partial unique index). A second participant is refused (409 CONFLICT) until
//!   the presenter stops; the presenter repeating its start is an idempotent no-op.
//! * Only the presenter's provider grant includes the screen sources (SCREEN_SHARE,
//!   SCREEN_SHARE_AUDIO): the grant is pushed after commit and HOLD masks it. Stopping, the
//!   presenter's last session ending (leave / removal / transfer) or the Call ending clears
//!   the state and the source is revoked (a failed push is repaired by reconciliation).
//! * Evidence: CALL_SCREEN_SHARE_STARTED / CALL_SCREEN_SHARE_STOPPED (with the stop
//!   reason); signals `call.screen_share_started` / `call.screen_share_stopped` carry the
//!   presenter participant id. The Call view shows the presenter (`screenShare`).
//!
//! Authority: the application backend (`calls.write`) names the presenting participant;
//! the CF Meet web adapter injects the signed-in user's own active participant.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{ringing::replay, summary, AppCallService, CommandOutcome, PendingSignals};
use crate::{
    calls::{
        command::CallCommandContext,
        errors::{CallError, CallResult},
        ids::{CallId, CallParticipantId},
        kinds::{CommandOperation, MediaAccess, SessionState},
        service::{participant_of, CommandKey},
        state::CallState,
    },
    models::{call_sessions, calls},
    signaling::signals::SignalType,
};

fn time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The presenter as the public API shows it (`null` when nobody presents).
pub(crate) fn screen_share_view(presenter: Option<(Uuid, chrono::DateTime<chrono::Utc>)>) -> Value {
    match presenter {
        Some((participant, since)) => {
            json!({ "participantId": participant, "startedAt": time(since) })
        }
        None => Value::Null,
    }
}

/// States in which a presentation may start (HOLD would mask it; pre-answer has no media
/// for the other parties yet).
pub(crate) fn screen_share_state(state: CallState) -> bool {
    matches!(state, CallState::Answered | CallState::Connected)
}

impl AppCallService {
    /// `POST /v1/calls/{id}/screen-share` (C6): `participant` starts presenting.
    pub async fn start_screen_share(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        participant: CallParticipantId,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let scope = cmd.scope();
        if let Some(key) = key {
            if let Some(prior) = self.calls.prior_command(self.db(), scope, key).await? {
                return replay(prior);
            }
        }
        if !self.config.screen_share_enabled {
            return Err(CallError::CapabilityUnavailable(
                "screen sharing is disabled",
            ));
        }
        let provider = self.provider()?.clone();
        if !provider.capabilities().permission_update {
            return Err(CallError::CapabilityUnavailable(
                "screen sharing needs server-side media permission updates",
            ));
        }
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        let participant = participant_of(&txn, &call, participant).await?;
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !screen_share_state(call.current_state) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: CommandOperation::StartScreenShare.as_str(),
            });
        }
        if participant.ended_at.is_some() {
            return Err(CallError::InvalidState("the participant has left the call"));
        }
        let live_session = call_sessions::Entity::find()
            .filter(call_sessions::Column::CallId.eq(call.id))
            .filter(call_sessions::Column::ParticipantId.eq(participant.id))
            .filter(call_sessions::Column::MediaAccess.eq(MediaAccess::Authorized))
            .filter(call_sessions::Column::State.ne(SessionState::Ended))
            .one(&txn)
            .await?;
        if live_session.is_none() {
            return Err(CallError::InvalidState(
                "the participant has no live media session",
            ));
        }
        self.require_personal_call(&call, None).await?;
        let started = self
            .calls
            .start_screen_share_in(&txn, cmd, &call, &participant)
            .await?;
        let presenter = self.calls.presenter_in(&txn, call.id).await?;
        let body = screen_share_body(&call, CommandOperation::StartScreenShare, presenter);
        if !started {
            txn.commit().await?;
            return Ok(CommandOutcome {
                body,
                replayed: true,
            });
        }
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    Some(participant.id),
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
        self.screen_share_signal_in(
            &txn,
            &mut pending,
            SignalType::ScreenShareStarted,
            &call,
            participant.id,
            cmd.correlation_id().as_str(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::inc("calling_screen_shares_total", &[("action", "STARTED")]);
        self.push_effective_permissions(&call, "SCREEN_SHARE_STARTED")
            .await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// `POST /v1/calls/{id}/screen-share/stop` (C6): the presenter stops. Stopping when
    /// nobody presents is an idempotent no-op; another participant cannot stop the
    /// presenter (409 CONFLICT).
    pub async fn stop_screen_share(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        participant: CallParticipantId,
        key: Option<&CommandKey>,
    ) -> CallResult<CommandOutcome> {
        let scope = cmd.scope();
        let txn = self.db().begin().await?;
        if let Some(key) = key {
            if let Some(prior) = self.calls.begin_command_in(&txn, scope, key).await? {
                return replay(prior);
            }
        }
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        let participant = participant_of(&txn, &call, participant).await?;
        match self.calls.presenter_in(&txn, call.id).await? {
            None => {
                let body = screen_share_body(&call, CommandOperation::StopScreenShare, None);
                txn.commit().await?;
                return Ok(CommandOutcome {
                    body,
                    replayed: true,
                });
            }
            Some((presenter, _)) if presenter != participant.id => {
                return Err(CallError::Conflict(
                    "only the presenter can stop its screen share",
                ));
            }
            Some(_) => {}
        }
        self.calls
            .clear_screen_share_in(&txn, cmd, &call, Some(participant.id), "STOPPED")
            .await?;
        let body = screen_share_body(&call, CommandOperation::StopScreenShare, None);
        if let Some(key) = key {
            self.calls
                .record_command_in(
                    &txn,
                    cmd,
                    key,
                    Some(call.id),
                    Some((200, &body)),
                    Some(call.version),
                    Some(participant.id),
                )
                .await?;
        }
        let mut pending = PendingSignals::default();
        self.screen_share_signal_in(
            &txn,
            &mut pending,
            SignalType::ScreenShareStopped,
            &call,
            participant.id,
            cmd.correlation_id().as_str(),
        )
        .await?;
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::inc("calling_screen_shares_total", &[("action", "STOPPED")]);
        self.push_effective_permissions(&call, "SCREEN_SHARE_STOPPED")
            .await;
        Ok(CommandOutcome {
            body,
            replayed: false,
        })
    }

    /// `call.screen_share_started` / `call.screen_share_stopped` to every endpoint of the
    /// Call (data: the presenter's participant id).
    pub(crate) async fn screen_share_signal_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        pending: &mut PendingSignals,
        kind: SignalType,
        call: &calls::Model,
        presenter: Uuid,
        correlation_id: &str,
    ) -> CallResult<()> {
        let mut data = Map::new();
        data.insert("participantId".into(), json!(presenter));
        self.call_signal(txn, pending, kind, call, correlation_id, data)
            .await
    }
}

fn screen_share_body(
    call: &calls::Model,
    operation: CommandOperation,
    presenter: Option<(Uuid, chrono::DateTime<chrono::Utc>)>,
) -> Value {
    json!({
        "call": summary(call),
        "operation": operation.as_str(),
        "screenShare": screen_share_view(presenter),
    })
}
