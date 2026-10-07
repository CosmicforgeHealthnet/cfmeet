//! Ring offers and single-winner answer arbitration (Part C §20) — transaction-scoped
//! primitives composed by the application-call orchestration.
//!
//! * One invitation per selected endpoint, created PENDING and moved to RINGING in the
//!   same transaction, with server-time expiry. Ringing several endpoints of one
//!   principal never creates a second participant: every invitation points at the one
//!   participant of that principal.
//! * Arbitration: [`CallService::claim_answer_in`] inserts the participant's claim with
//!   `ON CONFLICT ON CONSTRAINT uq_call_answer_claims_participant DO NOTHING`. Exactly one
//!   claim can exist per (Call, participant) whatever the interleaving; the caller holds
//!   the Call row lock as well, so in practice acceptances of one Call are serialized and
//!   the loser observes ANSWERED_ELSEWHERE before it even tries.

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseTransaction,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use uuid::Uuid;

use super::{data, scalar_i64, text, CallService, EventDraft};
use crate::{
    calls::{
        attributes::SafeValue,
        command::CallCommandContext,
        errors::{CallError, CallResult},
        events::CallEventType,
        ids::{CallAnswerClaimId, CallInvitationId},
        kinds::{InvitationPurpose, InvitationStatus, InvitationTerminalReason},
    },
    models::{call_endpoints, call_invitations, call_participants, calls},
};

impl CallService {
    /// Issues one ring offer per endpoint for `participant` (PENDING → RINGING), with a
    /// CALL_INVITATION_ISSUED event each. Endpoints must be ACTIVE endpoints of the
    /// participant's principal (also database-enforced).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn issue_invitations_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        participant: &call_participants::Model,
        endpoints: &[call_endpoints::Model],
        purpose: InvitationPurpose,
        transfer_id: Option<Uuid>,
        ttl: Duration,
    ) -> CallResult<Vec<call_invitations::Model>> {
        let mut out = Vec::with_capacity(endpoints.len());
        for endpoint in endpoints {
            if endpoint.principal_id != participant.principal_id {
                return Err(CallError::NotFound);
            }
            let generation = scalar_i64(
                txn,
                "SELECT (COALESCE(MAX(invitation_generation), 0) + 1)::bigint FROM call_invitations \
                 WHERE call_id = $1 AND endpoint_id = $2",
                vec![call.id.into(), endpoint.id.into()],
            )
            .await?;
            let generation = i32::try_from(generation)
                .map_err(|_| CallError::InvalidState("too many invitations"))?;
            let at = self.now().max(call.updated_at);
            let pending = call_invitations::ActiveModel {
                id: Set(CallInvitationId::generate().as_uuid()),
                call_id: Set(call.id),
                participant_id: Set(participant.id),
                principal_id: Set(participant.principal_id),
                endpoint_id: Set(endpoint.id),
                tenant_id: Set(call.tenant_id),
                application_id: Set(call.application_id),
                environment_id: Set(call.environment_id),
                session_id: Set(None),
                transfer_id: Set(transfer_id),
                purpose: Set(purpose),
                status: Set(InvitationStatus::Pending),
                invitation_generation: Set(generation),
                issued_at: Set(at),
                expires_at: Set(at + ttl),
                ringing_at: Set(None),
                accepted_at: Set(None),
                declined_at: Set(None),
                cancelled_at: Set(None),
                finished_at: Set(None),
                terminal_reason: Set(None),
                created_at: Set(at),
                updated_at: Set(at),
            }
            .insert(txn)
            .await?;
            let mut active: call_invitations::ActiveModel = pending.into();
            active.status = Set(InvitationStatus::Ringing);
            active.ringing_at = Set(Some(at));
            let invitation = active.update(txn).await?;
            let mut details = vec![
                ("invitation_id", text(invitation.id)),
                ("participant_id", text(invitation.participant_id)),
                ("endpoint_id", text(invitation.endpoint_id)),
                ("purpose", text(invitation.purpose)),
                (
                    "invitation_generation",
                    SafeValue::Integer(generation.into()),
                ),
                (
                    "expires_at",
                    text(
                        invitation
                            .expires_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                    ),
                ),
            ];
            if let Some(transfer_id) = transfer_id {
                details.push(("transfer_id", text(transfer_id)));
            }
            self.append_event(
                txn,
                cmd,
                call,
                EventDraft::fact(CallEventType::CallInvitationIssued, data(details)?),
                at,
            )
            .await?;
            out.push(invitation);
        }
        Ok(out)
    }

    /// Locks invitation `id` of `call` (`FOR UPDATE`); else not found.
    pub(crate) async fn lock_invitation(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        id: CallInvitationId,
    ) -> CallResult<call_invitations::Model> {
        call_invitations::Entity::find_by_id(id.as_uuid())
            .filter(call_invitations::Column::CallId.eq(call.id))
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(CallError::NotFound)
    }

    /// Live invitations of `call` (optionally of one participant), oldest first.
    pub(crate) async fn live_invitations(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        participant_id: Option<Uuid>,
    ) -> CallResult<Vec<call_invitations::Model>> {
        let mut query = call_invitations::Entity::find()
            .filter(call_invitations::Column::CallId.eq(call.id))
            .filter(
                call_invitations::Column::Status
                    .is_in([InvitationStatus::Pending, InvitationStatus::Ringing]),
            );
        if let Some(participant_id) = participant_id {
            query = query.filter(call_invitations::Column::ParticipantId.eq(participant_id));
        }
        Ok(query
            .order_by_asc(call_invitations::Column::IssuedAt)
            .order_by_asc(call_invitations::Column::Id)
            .all(txn)
            .await?)
    }

    /// Finishes every live invitation of `call` (of one participant, except one, when
    /// given) with `status` / `reason`. Returns the finished invitations.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_live_invitations_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        participant_id: Option<Uuid>,
        except: Option<Uuid>,
        status: InvitationStatus,
        reason: InvitationTerminalReason,
    ) -> CallResult<Vec<call_invitations::Model>> {
        let mut out = Vec::new();
        for invitation in self.live_invitations(txn, call, participant_id).await? {
            if Some(invitation.id) == except {
                continue;
            }
            out.push(
                self.finish_invitation_in(txn, cmd, call, invitation, status, reason, None)
                    .await?,
            );
        }
        Ok(out)
    }

    /// Moves one invitation to a terminal status with its evidence event. `session_id` is
    /// the winning session (ACCEPTED only).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_invitation_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CallCommandContext,
        call: &calls::Model,
        invitation: call_invitations::Model,
        status: InvitationStatus,
        reason: InvitationTerminalReason,
        session_id: Option<Uuid>,
    ) -> CallResult<call_invitations::Model> {
        if !invitation.status.can_transition_to(status) || !status.is_terminal() {
            return Err(CallError::InvalidState("invitation transition not allowed"));
        }
        let at = self.now().max(invitation.updated_at);
        let mut active: call_invitations::ActiveModel = invitation.clone().into();
        active.status = Set(status);
        active.finished_at = Set(Some(at));
        active.terminal_reason = Set(Some(reason));
        active.updated_at = Set(at);
        let event_type = match status {
            InvitationStatus::Accepted => {
                active.accepted_at = Set(Some(at));
                active.session_id = Set(session_id);
                CallEventType::CallInvitationAccepted
            }
            InvitationStatus::Declined => {
                active.declined_at = Set(Some(at));
                CallEventType::CallInvitationDeclined
            }
            InvitationStatus::Cancelled => {
                active.cancelled_at = Set(Some(at));
                CallEventType::CallInvitationCancelled
            }
            InvitationStatus::AnsweredElsewhere => CallEventType::CallInvitationAnsweredElsewhere,
            InvitationStatus::Expired => CallEventType::CallInvitationExpired,
            InvitationStatus::Pending | InvitationStatus::Ringing => unreachable!("not terminal"),
        };
        let invitation = active.update(txn).await?;
        let mut details = vec![
            ("invitation_id", text(invitation.id)),
            ("participant_id", text(invitation.participant_id)),
            ("endpoint_id", text(invitation.endpoint_id)),
            ("purpose", text(invitation.purpose)),
            ("reason", text(reason)),
        ];
        if let Some(session_id) = invitation.session_id {
            details.push(("session_id", text(session_id)));
        }
        self.append_event(
            txn,
            cmd,
            call,
            EventDraft::fact(event_type, data(details)?),
            at,
        )
        .await?;
        Ok(invitation)
    }

    /// The single-winner claim of `participant_id`'s answer. `true` if this invitation /
    /// session won; `false` if the participant already has a winner (database-enforced:
    /// `uq_call_answer_claims_participant`).
    pub(crate) async fn claim_answer_in(
        &self,
        txn: &DatabaseTransaction,
        call: &calls::Model,
        participant_id: Uuid,
        invitation_id: Uuid,
        session_id: Uuid,
        at: DateTime<Utc>,
    ) -> CallResult<bool> {
        let inserted = txn
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "INSERT INTO call_answer_claims \
                    (id, call_id, participant_id, tenant_id, application_id, environment_id, \
                     winner_invitation_id, winner_session_id, claimed_at, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
                 ON CONFLICT ON CONSTRAINT uq_call_answer_claims_participant DO NOTHING \
                 RETURNING id",
                vec![
                    CallAnswerClaimId::generate().as_uuid().into(),
                    call.id.into(),
                    participant_id.into(),
                    call.tenant_id.into(),
                    call.application_id.into(),
                    call.environment_id.into(),
                    invitation_id.into(),
                    session_id.into(),
                    at.into(),
                ],
            ))
            .await?;
        Ok(inserted.is_some())
    }
}
