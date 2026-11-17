//! Rejoining an ongoing call (`POST /v1/calls/{id}/participants/{participant_id}/rejoin`).
//!
//! Leaving a call is the removal of one's sessions (`…/sessions/{id}/remove`): the
//! participant itself stays in the Call. A participant who joined before (it held a
//! session) may come back while the Call is live (ANSWERED / CONNECTED / HELD) with a new
//! session on one of its own ACTIVE endpoints; media credentials can then be issued for
//! that session exactly as after an answer. Refused:
//!
//! * a participant the host removed (or that otherwise left the Call for good,
//!   `ended_at` set): 403 CALL_NOT_AUTHORIZED — removal is final;
//! * a participant that never joined (its invitation was never accepted): 409 — it answers
//!   its invitation instead;
//! * a Call that is not live: 409; an endpoint that is not the participant's own active
//!   endpoint: 404 (like every foreign resource).
//!
//! A repeat while a live session on the same endpoint exists returns that session
//! (idempotent). Every other participant receives `call.participants_changed`
//! (`rejoinedParticipantId`). The Communications personal policy is re-checked exactly as
//! for an answer, and a recording in progress gives the rejoined participant its notice
//! (and requires its consent when the policy requires consent).
//!
//! Authority: the application backend (`calls.write`) names the participant; the CF Meet
//! web adapter injects the signed-in user's own participant.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{summary, AppCallService, PendingSignals};
use crate::{
    calls::{
        command::CallCommandContext,
        errors::{CallError, CallResult},
        ids::{CallId, CallParticipantId},
        kinds::{ParticipantRole, SessionState},
        service::participant_of,
        state::CallState,
    },
    domain::PrincipalId,
    endpoints::EndpointStatus,
    models::{call_answer_claims, call_endpoints, call_sessions},
    platform::principals::{PrincipalReference, PrincipalResolutionError},
    signaling::signals::SignalType,
};

/// Call states in which a participant may rejoin.
pub(crate) fn rejoinable_state(state: CallState) -> bool {
    matches!(
        state,
        CallState::Answered | CallState::Connected | CallState::Held
    )
}

impl AppCallService {
    /// `POST /v1/calls/{id}/participants/{participant_id}/rejoin`.
    pub async fn rejoin_call(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        participant_id: CallParticipantId,
        endpoint_id: Uuid,
    ) -> CallResult<Value> {
        let scope = cmd.scope();
        let txn = self.db().begin().await?;
        let call = self.calls.lock_call(&txn, scope, call_id).await?;
        let participant = participant_of(&txn, &call, participant_id).await?;
        if call.current_state.is_terminal() {
            return Err(CallError::Terminal {
                state: call.current_state,
            });
        }
        if !rejoinable_state(call.current_state) {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: "REJOIN",
            });
        }
        if participant.ended_at.is_some() {
            crate::metrics::inc("calling_rejoins_total", &[("result", "REMOVED")]);
            return Err(CallError::AuthorizationDenied);
        }
        let sessions = call_sessions::Entity::find()
            .filter(call_sessions::Column::ParticipantId.eq(participant.id))
            .all(&txn)
            .await?;
        // Joined before: the initiator held a session; a recipient won its answer. The new
        // session continues that participation (it replaces the latest session of the
        // winning lineage, so the database's media-authority rule holds).
        let lineage_root = if participant.role == ParticipantRole::Initiator {
            sessions.iter().min_by_key(|s| s.created_at).map(|s| s.id)
        } else {
            call_answer_claims::Entity::find()
                .filter(call_answer_claims::Column::ParticipantId.eq(participant.id))
                .one(&txn)
                .await?
                .map(|c| c.winner_session_id)
        };
        let Some(root) = lineage_root else {
            return Err(CallError::InvalidState(
                "the participant never joined the call; it answers its invitation instead",
            ));
        };
        let mut tip = root;
        while let Some(next) = sessions.iter().find(|s| s.replaces_session_id == Some(tip)) {
            tip = next.id;
        }
        let tip = sessions
            .iter()
            .find(|s| s.id == tip)
            .ok_or(CallError::NotFound)?;
        let endpoint = call_endpoints::Entity::find_by_id(endpoint_id)
            .one(&txn)
            .await?
            .filter(|e| {
                e.status == EndpointStatus::Active
                    && e.principal_id == participant.principal_id
                    && e.tenant_id == call.tenant_id
                    && e.application_id == call.application_id
                    && e.environment_id == call.environment_id
            })
            .ok_or(CallError::NotFound)?;
        // Idempotent: a live session on the same endpoint is the result.
        if let Some(live) = sessions.iter().find(|s| {
            s.endpoint_id == Some(endpoint.id)
                && s.state != SessionState::Ended
                && s.removed_at.is_none()
        }) {
            let body = rejoin_body(&call, participant.id, live.id, false);
            txn.commit().await?;
            return Ok(body);
        }
        if tip.state != SessionState::Ended && tip.removed_at.is_none() {
            return Err(CallError::InvalidState(
                "the participant is still in the call on another device",
            ));
        }
        // The participant must still be an active principal of the scope, and a personal
        // Call still allowed by the Communications policy.
        match self
            .principals
            .resolve_in(
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                &PrincipalReference::Id(PrincipalId::from_uuid(participant.principal_id)),
            )
            .await
        {
            Ok(_) => {}
            Err(PrincipalResolutionError::Unavailable { summary }) => {
                return Err(CallError::Database {
                    summary,
                    connectivity: true,
                })
            }
            Err(_) => return Err(CallError::AuthorizationDenied),
        }
        self.require_personal_call(&call, None).await?;
        let session = self
            .calls
            .insert_session(
                &txn,
                cmd,
                &call,
                &participant,
                endpoint.endpoint_kind.session_category(),
                Some(endpoint.id),
                Some(tip.id),
            )
            .await?;
        let session = self.calls.authorize_session_media_in(&txn, session).await?;
        self.calls
            .ensure_media_permissions_in(&txn, &call, &participant)
            .await?;
        let mut pending = PendingSignals::default();
        let mut data = Map::new();
        data.insert("rejoinedParticipantId".into(), json!(participant.id));
        self.call_signal(
            &txn,
            &mut pending,
            SignalType::ParticipantsChanged,
            &call,
            cmd.correlation_id().as_str(),
            data,
        )
        .await?;
        let body = rejoin_body(&call, participant.id, session.id, true);
        txn.commit().await?;
        self.publish(pending).await;
        crate::metrics::inc("calling_rejoins_total", &[("result", "REJOINED")]);
        tracing::info!(call_id = %call.id, participant_id = %participant.id, session_id = %session.id, "participant rejoined the call");
        // A recording in progress: notice (and consent) for the rejoined participant.
        self.recording_notice_on_join(call.id, participant.id).await;
        Ok(body)
    }
}

fn rejoin_body(
    call: &crate::models::calls::Model,
    participant: Uuid,
    session: Uuid,
    rejoined: bool,
) -> Value {
    json!({
        "call": summary(call),
        "operation": "REJOIN",
        "participantId": participant,
        "sessionId": session,
        "rejoined": rejoined,
    })
}
