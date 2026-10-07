//! Participants, sessions and connection intervals (Part C §19).

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter,
    QueryOrder, TransactionTrait,
};

use super::{data, text, CallService, EventDraft};
use crate::{
    calls::{
        attributes::SafeValue,
        command::{CallCommandContext, CallScope},
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallId, CallParticipantId, CallSessionId, CallSessionIntervalId, MediaIdentity},
        kinds::{DisconnectReason, EndpointCategory, ParticipantRole, SessionState},
        participants::{CallParticipant, DisplayName},
        sessions::{CallSession, CallSessionInterval},
        state::CallState,
    },
    models::{call_participants, call_session_intervals, call_sessions, calls},
    platform::principals::ResolvedPrincipal,
};

impl CallService {
    /// Adds a trusted principal as a RECIPIENT participant of a non-terminal Call.
    ///
    /// The principal must have been resolved (by `PrincipalResolver`) in the command's own
    /// scope; a principal of another tenant / application / environment is reported as
    /// not found. Adding a principal that already participates is idempotent and returns
    /// the existing participant (`false`); the database guarantees one participant per
    /// principal per Call even under concurrent insertion.
    pub async fn add_participant(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        principal: &ResolvedPrincipal,
        display_name: Option<DisplayName>,
    ) -> CallResult<(CallParticipant, bool)> {
        if CallScope::of_principal(principal) != cmd.scope() {
            return Err(CallError::NotFound);
        }
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        if let Some(existing) = call_participants::Entity::find()
            .filter(call_participants::Column::CallId.eq(call.id))
            .filter(call_participants::Column::PrincipalId.eq(principal.id().as_uuid()))
            .one(&txn)
            .await?
        {
            txn.commit().await?;
            return Ok((existing.into(), false));
        }
        let at = self.now().max(call.updated_at);
        let participant = call_participants::ActiveModel {
            id: Set(CallParticipantId::generate().as_uuid()),
            call_id: Set(call.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            principal_id: Set(principal.id().as_uuid()),
            principal_kind: Set(principal.kind()),
            role: Set(ParticipantRole::Recipient),
            display_name: Set(display_name.as_ref().map(|d| d.as_str().to_string())),
            created_at: Set(at),
            ended_at: Set(None),
            updated_at: Set(at),
        }
        .insert(&txn)
        .await?;
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(
                CallEventType::CallParticipantAdded,
                data(vec![
                    ("participant_id", text(participant.id)),
                    ("principal_kind", text(participant.principal_kind)),
                    ("role", text(participant.role)),
                    (
                        "display_name_set",
                        SafeValue::Bool(participant.display_name.is_some()),
                    ),
                ])?,
            ),
            at,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            participant_id = %participant.id,
            request_id = %cmd.request_id(),
            "call participant added"
        );
        Ok((participant.into(), true))
    }

    /// Changes a participant's presentation-only display name. Identity, role and
    /// authority are unchanged.
    pub async fn set_participant_display_name(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        participant_id: CallParticipantId,
        display_name: Option<DisplayName>,
    ) -> CallResult<CallParticipant> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let participant = participant_of(&txn, &call, participant_id).await?;
        let at = self.now().max(participant.updated_at);
        let mut active: call_participants::ActiveModel = participant.into();
        active.display_name = Set(display_name.as_ref().map(|d| d.as_str().to_string()));
        active.updated_at = Set(at);
        let participant = active.update(&txn).await?;
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(
                CallEventType::CallParticipantUpdated,
                data(vec![
                    ("participant_id", text(participant.id)),
                    (
                        "display_name_set",
                        SafeValue::Bool(participant.display_name.is_some()),
                    ),
                ])?,
            ),
            at,
        )
        .await?;
        txn.commit().await?;
        Ok(participant.into())
    }

    /// Creates a new session (device / network participation) for a participant of a
    /// non-terminal Call. The media identity is generated here; it is never an input.
    /// A participant may hold several sessions at the same time.
    pub async fn create_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        participant_id: CallParticipantId,
        endpoint_category: EndpointCategory,
    ) -> CallResult<CallSession> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let participant = participant_of(&txn, &call, participant_id).await?;
        let session = self
            .insert_session(&txn, cmd, &call, &participant, endpoint_category, None)
            .await?;
        txn.commit().await?;
        Ok(session.into())
    }

    /// Replacement reconnect: the connection / device identity changed. Ends `old` (closing
    /// its open interval as SESSION_REPLACED) and creates a new session with a new media
    /// identity for the same participant. The Call and the participant are unchanged.
    pub async fn replace_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        old_session_id: CallSessionId,
        endpoint_category: EndpointCategory,
    ) -> CallResult<CallSession> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        let old = session_of(&txn, &call, old_session_id).await?;
        let participant = participant_of(
            &txn,
            &call,
            CallParticipantId::from_uuid(old.participant_id),
        )
        .await?;
        if old.state != SessionState::Ended {
            self.finish_session(
                &txn,
                cmd,
                &call,
                old.clone(),
                DisconnectReason::SessionReplaced,
            )
            .await?;
        }
        let session = self
            .insert_session(
                &txn,
                cmd,
                &call,
                &participant,
                endpoint_category,
                Some(old.id),
            )
            .await?;
        txn.commit().await?;
        Ok(session.into())
    }

    /// First connection of a CREATED session: opens connection interval 1.
    pub async fn connect_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
    ) -> CallResult<(CallSession, CallSessionInterval)> {
        self.open_interval(cmd, call_id, session_id, SessionState::Created)
            .await
    }

    /// Renewal reconnect of a DISCONNECTED session: opens connection interval n + 1 on the
    /// same session. The Call, the participant and the session identity are unchanged.
    pub async fn reconnect_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
    ) -> CallResult<(CallSession, CallSessionInterval)> {
        self.open_interval(cmd, call_id, session_id, SessionState::Disconnected)
            .await
    }

    /// Closes the open interval of a CONNECTED session. Allowed reasons: LEFT,
    /// CONNECTION_LOST, TIMEOUT (the others are recorded by the service itself).
    pub async fn disconnect_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
        reason: DisconnectReason,
    ) -> CallResult<(CallSession, CallSessionInterval)> {
        if !matches!(
            reason,
            DisconnectReason::Left | DisconnectReason::ConnectionLost | DisconnectReason::Timeout
        ) {
            return Err(CallError::Validation("this disconnect reason is reserved"));
        }
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let session = session_of(&txn, &call, session_id).await?;
        if session.state != SessionState::Connected {
            return Err(CallError::InvalidState("the session is not connected"));
        }
        let at = self.now().max(session.updated_at);
        let interval = close_open_interval(&txn, session.id, reason, at)
            .await?
            .ok_or(CallError::InvalidState("the session has no open interval"))?;
        let mut active: call_sessions::ActiveModel = session.into();
        active.state = Set(SessionState::Disconnected);
        active.last_disconnected_at = Set(Some(at));
        active.updated_at = Set(at);
        let session = active.update(&txn).await?;
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(
                CallEventType::CallSessionDisconnected,
                data(vec![
                    ("session_id", text(session.id)),
                    ("participant_id", text(session.participant_id)),
                    (
                        "interval_number",
                        SafeValue::Integer(interval.interval_number.into()),
                    ),
                    ("disconnect_reason", text(reason)),
                ])?,
            ),
            at,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            session_id = %session.id,
            participant_id = %session.participant_id,
            "call session disconnected"
        );
        Ok((session.into(), interval.into()))
    }

    /// Ends a session for good (closing its open interval as SESSION_ENDED).
    pub async fn end_session(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
    ) -> CallResult<CallSession> {
        let txn = self.db.begin().await?;
        let call = self.lock_call(&txn, cmd.scope(), call_id).await?;
        let session = session_of(&txn, &call, session_id).await?;
        if session.state == SessionState::Ended {
            return Err(CallError::InvalidState("the session has ended"));
        }
        let session = self
            .finish_session(&txn, cmd, &call, session, DisconnectReason::SessionEnded)
            .await?;
        txn.commit().await?;
        Ok(session.into())
    }

    async fn insert_session(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        participant: &call_participants::Model,
        endpoint_category: EndpointCategory,
        replaces: Option<uuid::Uuid>,
    ) -> CallResult<call_sessions::Model> {
        if participant.ended_at.is_some() {
            return Err(CallError::InvalidState("the participant has left the call"));
        }
        let at = self.now().max(participant.updated_at).max(call.updated_at);
        let session = call_sessions::ActiveModel {
            id: Set(CallSessionId::generate().as_uuid()),
            call_id: Set(call.id),
            participant_id: Set(participant.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            media_identity: Set(MediaIdentity::generate().as_str().to_string()),
            endpoint_category: Set(endpoint_category),
            state: Set(SessionState::Created),
            generation: Set(0),
            replaces_session_id: Set(replaces),
            created_at: Set(at),
            first_connected_at: Set(None),
            last_disconnected_at: Set(None),
            ended_at: Set(None),
            updated_at: Set(at),
        }
        .insert(txn)
        .await?;
        let mut details = vec![
            ("session_id", text(session.id)),
            ("participant_id", text(session.participant_id)),
            ("endpoint_category", text(session.endpoint_category)),
        ];
        if let Some(replaces) = replaces {
            details.push(("replaces_session_id", text(replaces)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallSessionCreated, data(details)?),
            at,
        )
        .await?;
        tracing::info!(
            call_id = %call.id,
            session_id = %session.id,
            participant_id = %session.participant_id,
            "call session created"
        );
        Ok(session)
    }

    async fn open_interval(
        &self,
        cmd: &CallCommandContext,
        call_id: CallId,
        session_id: CallSessionId,
        from: SessionState,
    ) -> CallResult<(CallSession, CallSessionInterval)> {
        let txn = self.db.begin().await?;
        let call = self.lock_open_call(&txn, cmd.scope(), call_id).await?;
        if call.current_state == CallState::Ending {
            return Err(CallError::InvalidTransition {
                state: call.current_state,
                action: "CONNECT_SESSION",
            });
        }
        let session = session_of(&txn, &call, session_id).await?;
        if session.state != from {
            return Err(CallError::InvalidState(match from {
                SessionState::Created => "only a new session connects; use reconnect",
                _ => "only a disconnected session reconnects",
            }));
        }
        let at = self.now().max(session.updated_at);
        let number = session.generation + 1;
        let interval = call_session_intervals::ActiveModel {
            id: Set(CallSessionIntervalId::generate().as_uuid()),
            call_id: Set(call.id),
            session_id: Set(session.id),
            tenant_id: Set(call.tenant_id),
            application_id: Set(call.application_id),
            environment_id: Set(call.environment_id),
            interval_number: Set(number),
            connected_at: Set(at),
            disconnected_at: Set(None),
            disconnect_reason: Set(None),
            created_at: Set(at),
        }
        .insert(&txn)
        .await?;
        let first = session.first_connected_at.is_none();
        let mut active: call_sessions::ActiveModel = session.into();
        active.state = Set(SessionState::Connected);
        active.generation = Set(number);
        if first {
            active.first_connected_at = Set(Some(at));
        }
        active.updated_at = Set(at);
        let session = active.update(&txn).await?;
        let event_type = if from == SessionState::Created {
            CallEventType::CallSessionConnected
        } else {
            CallEventType::CallSessionReconnected
        };
        self.append_event(
            &txn,
            cmd,
            &call,
            EventDraft::fact(
                event_type,
                data(vec![
                    ("session_id", text(session.id)),
                    ("participant_id", text(session.participant_id)),
                    ("interval_number", SafeValue::Integer(number.into())),
                ])?,
            ),
            at,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            call_id = %call.id,
            session_id = %session.id,
            participant_id = %session.participant_id,
            interval_number = number,
            "call session connected"
        );
        Ok((session.into(), interval.into()))
    }

    /// Ends `session`: closes its open interval with `reason`, marks it ENDED, emits
    /// CALL_SESSION_ENDED.
    async fn finish_session(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        session: call_sessions::Model,
        reason: DisconnectReason,
    ) -> CallResult<call_sessions::Model> {
        let at = self.now().max(session.updated_at);
        let closed = close_open_interval(txn, session.id, reason, at).await?;
        let was_connected = session.state == SessionState::Connected;
        let mut active: call_sessions::ActiveModel = session.into();
        active.state = Set(SessionState::Ended);
        active.ended_at = Set(Some(at));
        if was_connected {
            active.last_disconnected_at = Set(Some(at));
        }
        active.updated_at = Set(at);
        let session = active.update(txn).await?;
        let mut details = vec![
            ("session_id", text(session.id)),
            ("participant_id", text(session.participant_id)),
            ("end_reason", text(reason)),
        ];
        if let Some(interval) = &closed {
            details.push((
                "interval_number",
                SafeValue::Integer(interval.interval_number.into()),
            ));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(CallEventType::CallSessionEnded, data(details)?),
            at,
        )
        .await?;
        Ok(session)
    }
}

/// Participant `id` of `call` (else not found).
pub(super) async fn participant_of(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    id: CallParticipantId,
) -> CallResult<call_participants::Model> {
    call_participants::Entity::find_by_id(id.as_uuid())
        .filter(call_participants::Column::CallId.eq(call.id))
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)
}

/// Session `id` of `call` (else not found).
pub(super) async fn session_of(
    txn: &DatabaseTransaction,
    call: &calls::Model,
    id: CallSessionId,
) -> CallResult<call_sessions::Model> {
    call_sessions::Entity::find_by_id(id.as_uuid())
        .filter(call_sessions::Column::CallId.eq(call.id))
        .one(txn)
        .await?
        .ok_or(CallError::NotFound)
}

/// Closes the session's open interval, if any.
async fn close_open_interval(
    txn: &DatabaseTransaction,
    session_id: uuid::Uuid,
    reason: DisconnectReason,
    at: DateTime<Utc>,
) -> CallResult<Option<call_session_intervals::Model>> {
    let Some(open) = call_session_intervals::Entity::find()
        .filter(call_session_intervals::Column::SessionId.eq(session_id))
        .filter(call_session_intervals::Column::DisconnectedAt.is_null())
        .order_by_desc(call_session_intervals::Column::IntervalNumber)
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let at = at.max(open.connected_at);
    let mut active: call_session_intervals::ActiveModel = open.into();
    active.disconnected_at = Set(Some(at));
    active.disconnect_reason = Set(Some(reason));
    Ok(Some(active.update(txn).await?))
}
