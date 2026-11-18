//! C4 — the host of a call (migration 000052).
//!
//! * **Who.** The host is the participant the role was last transferred to
//!   (`call_participants.is_host`, at most one per Call). Without one it is the Call's
//!   INITIATOR (its creator), so every Call, including those created before the host
//!   existed, has the creator as host. When the host has left the Call for good (removed,
//!   transferred away) the role falls to the initiator, then to the longest-standing
//!   active participant: a live Call always has a host.
//! * **Host-only in a group Call.** A Call with more than two participants (ever) is a
//!   group Call. END (for everyone) and HOLD / RESUME (everyone) name the acting
//!   participant (`actingParticipantId`); a non-host is refused (403 CALL_NOT_AUTHORIZED)
//!   and leaves instead (session removal). In a one-to-one Call either party may end or
//!   hold, as before. A request without `actingParticipantId` is the application backend
//!   acting on its own authority (unchanged, backward compatible); the CF Meet web adapter
//!   injects the signed-in user's own participant id, as it does for invitations.
//! * **Host operations** (the acting participant must be the host, in every Call):
//!   transfer the host role (`POST /v1/calls/{id}/host`), remove a participant
//!   (`POST …/participants/{participant_id}/remove`: retired with reason HOST_REMOVED —
//!   its sessions end, its media is revoked at the provider, its live ring offers are
//!   cancelled, and it can never rejoin; the Call ends automatically when fewer than two
//!   parties remain, CAL-001) and mute / unmute a participant
//!   (`POST …/participants/{participant_id}/mute`: no audio publishing, enforced at the
//!   provider, until the host unmutes; it survives hold / resume).
//! * Every change is a Call event in the same transaction and a
//!   `call.participants_changed` signal (`hostParticipantId`, `removedParticipantId` or
//!   `mutedParticipantId` + `muted`) to every endpoint of the Call.

use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, TransactionTrait};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{enterprise::stmt, summary, AppCallService, PendingSignals};
use crate::{
    calls::{
        attributes::SafeValue,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallId, CallParticipantId},
        kinds::{InvitationStatus, InvitationTerminalReason},
        service::{data, effective_host_in, participant_of, text, EventDraft},
        state::CallState,
    },
    models::{call_participant_media_permissions, call_participants, calls},
    signaling::signals::SignalType,
};

/// Whether the Call is a group Call: more than two participants, ever.
pub(crate) async fn is_group_in<C: ConnectionTrait>(db: &C, call_id: Uuid) -> CallResult<bool> {
    let n: i64 = db
        .query_one(stmt(
            "SELECT count(*) FROM call_participants WHERE call_id = $1",
            vec![call_id.into()],
        ))
        .await?
        .map(|r| r.try_get_by_index(0))
        .transpose()?
        .unwrap_or(0);
    Ok(n > 2)
}

/// Live Call states in which the host operates on participants.
fn live(state: CallState) -> bool {
    matches!(
        state,
        CallState::Answered | CallState::Connected | CallState::Held
    )
}

impl AppCallService {
    /// END / HOLD / RESUME of a group Call by a named participant: only its host (C4).
    /// `None` = the application backend acting on its own authority (unchanged).
    pub async fn require_host_for_everyone(
        &self,
        scope: CallScope,
        call_id: CallId,
        acting: Option<CallParticipantId>,
    ) -> CallResult<()> {
        let Some(acting) = acting else {
            return Ok(());
        };
        let call = self.calls_row(scope, call_id).await?;
        let txn = self.db().begin().await?;
        let participant = participant_of(&txn, &call, acting).await?;
        if participant.ended_at.is_some() {
            return Err(CallError::InvalidState(
                "the acting participant has left the call",
            ));
        }
        if is_group_in(&txn, call.id).await?
            && effective_host_in(&txn, call.id).await? != Some(participant.id)
        {
            crate::metrics::inc("calling_host_refusals_total", &[("action", "EVERYONE")]);
            return Err(CallError::AuthorizationDenied);
        }
        txn.commit().await?;
        Ok(())
    }

    /// Locks the Call and checks that `acting` is its host (every host operation).
    async fn host_txn(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        acting: CallParticipantId,
    ) -> CallResult<(
        sea_orm::DatabaseTransaction,
        calls::Model,
        call_participants::Model,
    )> {
        let txn = self.db().begin().await?;
        let call = self.calls.lock_call(&txn, cmd.scope(), call_id).await?;
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !live(call.current_state) {
            return Err(CallError::InvalidState("the call is not live"));
        }
        let host = participant_of(&txn, &call, acting).await?;
        if host.ended_at.is_some() || effective_host_in(&txn, call.id).await? != Some(host.id) {
            crate::metrics::inc("calling_host_refusals_total", &[("action", "HOST_ONLY")]);
            return Err(CallError::AuthorizationDenied);
        }
        Ok((txn, call, host))
    }

    async fn host_signal(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        call: &calls::Model,
        cmd: &CallCommandContext,
        data: Map<String, Value>,
    ) -> CallResult<PendingSignals> {
        let mut pending = PendingSignals::default();
        self.call_signal(
            txn,
            &mut pending,
            SignalType::ParticipantsChanged,
            call,
            cmd.correlation_id().as_str(),
            data,
        )
        .await?;
        Ok(pending)
    }

    /// `POST /v1/calls/{id}/host`: the host hands the role to another active participant.
    pub async fn transfer_host(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        acting: CallParticipantId,
        new_host: CallParticipantId,
    ) -> CallResult<Value> {
        let (txn, call, host) = self.host_txn(cmd, call_id, acting).await?;
        let target = participant_of(&txn, &call, new_host).await?;
        if target.ended_at.is_some() {
            return Err(CallError::InvalidState("the new host has left the call"));
        }
        if target.id != host.id {
            let at = self.calls.now().max(call.updated_at);
            super::enterprise::exec(
                &txn,
                "UPDATE call_participants SET is_host = false, updated_at = GREATEST(updated_at, $2) \
                  WHERE call_id = $1 AND is_host",
                vec![call.id.into(), at.into()],
            )
            .await?;
            super::enterprise::exec(
                &txn,
                "UPDATE call_participants SET is_host = true, updated_at = GREATEST(updated_at, $2) \
                  WHERE id = $1",
                vec![target.id.into(), at.into()],
            )
            .await?;
            self.calls
                .append_event(
                    &txn,
                    cmd,
                    &call,
                    EventDraft::fact(
                        CallEventType::CallParticipantUpdated,
                        data(vec![
                            ("participant_id", text(target.id)),
                            ("change", text("HOST_ASSIGNED")),
                            ("previous_host_participant_id", text(host.id)),
                        ])?,
                    ),
                    at,
                )
                .await?;
        }
        let mut signal = Map::new();
        signal.insert("hostParticipantId".into(), json!(target.id));
        let pending = self.host_signal(&txn, &call, cmd, signal).await?;
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::inc("calling_host_actions_total", &[("action", "TRANSFER")]);
        tracing::info!(call_id = %call.id, host = %target.id, "call host transferred");
        Ok(json!({
            "call": summary(&call),
            "operation": "TRANSFER_HOST",
            "hostParticipantId": target.id,
        }))
    }

    /// `POST /v1/calls/{id}/participants/{participant_id}/remove`: the host removes a
    /// participant for good (it cannot rejoin).
    pub async fn remove_participant(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        acting: CallParticipantId,
        target: CallParticipantId,
    ) -> CallResult<Value> {
        let (txn, call, host) = self.host_txn(cmd, call_id, acting).await?;
        let participant = participant_of(&txn, &call, target).await?;
        if participant.id == host.id {
            return Err(CallError::Validation(
                "the host leaves the call instead of removing itself.",
            ));
        }
        let already_removed = participant.ended_at.is_some();
        let (participant, ended) = self
            .calls
            .retire_participant_in(&txn, cmd, &call, participant, "HOST_REMOVED")
            .await?;
        // Its live ring offers (e.g. an invitation not yet answered) stop ringing.
        let cancelled = self
            .calls
            .finish_live_invitations_in(
                &txn,
                cmd,
                &call,
                Some(participant.id),
                None,
                InvitationStatus::Cancelled,
                InvitationTerminalReason::ParticipantRemoved,
            )
            .await?;
        let active: i64 = txn
            .query_one(stmt(
                "SELECT count(*) FROM call_participants WHERE call_id = $1 AND ended_at IS NULL",
                vec![call.id.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index(0))
            .transpose()?
            .unwrap_or(0);
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
        if !already_removed {
            let mut signal = Map::new();
            signal.insert("removedParticipantId".into(), json!(participant.id));
            signal.insert("activeParticipants".into(), json!(active));
            self.call_signal(
                &txn,
                &mut pending,
                SignalType::ParticipantsChanged,
                &call,
                &correlation,
                signal,
            )
            .await?;
        }
        txn.commit().await?;
        self.publish(pending).await;
        self.enforce_removals(&call, ended).await;
        if !already_removed {
            crate::metrics::inc("calling_host_actions_total", &[("action", "REMOVE")]);
            tracing::info!(call_id = %call.id, participant = %participant.id, "participant removed by the host");
        }
        // CAL-001: fewer than two parties left ends the Call.
        self.evaluate_participation(call.id).await;
        Ok(json!({
            "call": summary(&self.locked(self.db(), &call).await?),
            "operation": "REMOVE_PARTICIPANT",
            "participantId": participant.id,
            "removed": true,
        }))
    }

    /// `POST /v1/calls/{id}/participants/{participant_id}/mute`: the host mutes (or
    /// unmutes) a participant's audio, enforced at the media provider.
    pub async fn mute_participant(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        acting: CallParticipantId,
        target: CallParticipantId,
        muted: bool,
    ) -> CallResult<Value> {
        let (txn, call, _host) = self.host_txn(cmd, call_id, acting).await?;
        let participant = participant_of(&txn, &call, target).await?;
        if participant.ended_at.is_some() {
            return Err(CallError::InvalidState("the participant has left the call"));
        }
        let permissions = self
            .calls
            .ensure_media_permissions_in(&txn, &call, &participant)
            .await?;
        let changed = permissions.host_muted_at.is_some() != muted;
        if changed {
            let at = self
                .calls
                .now()
                .max(permissions.updated_at)
                .max(call.updated_at);
            let revision = permissions.revision.saturating_add(1);
            let mut update: call_participant_media_permissions::ActiveModel = permissions.into();
            update.host_muted_at = Set(muted.then_some(at));
            update.revision = Set(revision);
            update.updated_at = Set(at);
            update.update(&txn).await?;
            self.calls
                .append_event(
                    &txn,
                    cmd,
                    &call,
                    EventDraft::fact(
                        CallEventType::CallMediaPermissionsUpdated,
                        data(vec![
                            (
                                "reason",
                                SafeValue::Text(
                                    if muted { "HOST_MUTED" } else { "HOST_UNMUTED" }.into(),
                                ),
                            ),
                            ("participant_id", text(participant.id)),
                        ])?,
                    ),
                    at,
                )
                .await?;
        }
        let mut signal = Map::new();
        signal.insert("mutedParticipantId".into(), json!(participant.id));
        signal.insert("muted".into(), json!(muted));
        let pending = if changed {
            self.host_signal(&txn, &call, cmd, signal).await?
        } else {
            PendingSignals::default()
        };
        txn.commit().await?;
        self.publish(pending).await;
        if changed {
            // Provider enforcement: every authorized session gets its effective
            // permissions again (the muted participant loses / regains audio publishing).
            if let Err(failure) = self
                .apply_effective_permissions(&call, call.current_state)
                .await
            {
                tracing::warn!(call_id = %call.id, %failure, "host mute enforcement deferred to reconciliation");
            }
            crate::metrics::inc(
                "calling_host_actions_total",
                &[("action", if muted { "MUTE" } else { "UNMUTE" })],
            );
        }
        Ok(json!({
            "call": summary(&call),
            "operation": if muted { "MUTE_PARTICIPANT" } else { "UNMUTE_PARTICIPANT" },
            "participantId": participant.id,
            "muted": muted,
        }))
    }
}
