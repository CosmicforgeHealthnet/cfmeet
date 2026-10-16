//! Scope-confined reads: call detail with participation durations, keyset-paginated
//! listing and per-aggregate lookups. Every query filters by the caller's tenant,
//! application and environment; a foreign Call is indistinguishable from a missing one.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sea_orm::{
    sea_query::{Expr, SimpleExpr},
    ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};

use super::CallService;
use crate::{
    calls::{
        aggregate::Call,
        attempts::CallAttempt,
        authorizations::CallAuthorization,
        command::CallScope,
        duration::{total_duration, union_duration, ConnectionSpan},
        errors::{CallError, CallResult},
        events::CallEvent,
        ids::{CallId, CallParticipantId},
        invitations::{CallInvitation, CallTransfer},
        legs::CallLeg,
        participants::CallParticipant,
        quality::CallQualityRecord,
        read_model::CallCursor,
        routes::CallRoute,
        sessions::{CallSession, CallSessionInterval},
        state::CallState,
        usage::CallUsageRecord,
    },
    domain::PrincipalId,
    models::{
        call_attempts, call_authorizations, call_events, call_invitations, call_legs,
        call_participants, call_quality_records, call_routes, call_session_intervals,
        call_sessions, call_transfers, call_usage_records, calls,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantDetail {
    pub participant: CallParticipant,
    /// Union of the participant's session intervals (no double counting), as of
    /// [`CallDetail::as_of`].
    pub connected_duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDetail {
    pub session: CallSession,
    pub connected_duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallDetail {
    pub call: Call,
    pub participants: Vec<ParticipantDetail>,
    pub sessions: Vec<SessionDetail>,
    pub invitations: Vec<CallInvitation>,
    pub transfers: Vec<CallTransfer>,
    /// From first CONNECTED to the end of the Call (or `as_of`); `None` if never connected.
    pub connected_duration_ms: Option<i64>,
    /// Instant open intervals were measured up to.
    pub as_of: DateTime<Utc>,
}

/// Bounded list filters. Scope always comes from the caller, never from here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListCallsQuery {
    pub limit: u64,
    pub cursor: Option<CallCursor>,
    pub state: Option<CallState>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub initiator_principal_id: Option<PrincipalId>,
    /// Only the protected Calls of this Privacy Relay Session (privacy-safe evidence).
    pub privacy_relay_ref: Option<uuid::Uuid>,
}

impl ListCallsQuery {
    pub const DEFAULT_LIMIT: u64 = 25;
    pub const MAX_LIMIT: u64 = 100;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPage {
    pub calls: Vec<Call>,
    pub next_cursor: Option<CallCursor>,
}

fn scoped(scope: CallScope) -> Condition {
    Condition::all()
        .add(calls::Column::TenantId.eq(scope.tenant_id().as_uuid()))
        .add(calls::Column::ApplicationId.eq(scope.application_id().as_uuid()))
        .add(calls::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
}

fn millis(d: chrono::Duration) -> i64 {
    d.num_milliseconds().max(0)
}

impl CallService {
    /// The Call, if it exists in `scope`.
    pub async fn call(&self, scope: CallScope, call_id: CallId) -> CallResult<Call> {
        Ok(self.call_row(scope, call_id).await?.into())
    }

    async fn call_row(&self, scope: CallScope, call_id: CallId) -> CallResult<calls::Model> {
        calls::Entity::find_by_id(call_id.as_uuid())
            .filter(scoped(scope))
            .one(&self.db)
            .await?
            .ok_or(CallError::NotFound)
    }

    /// Call detail with participants, sessions and durations measured up to now.
    pub async fn call_detail(&self, scope: CallScope, call_id: CallId) -> CallResult<CallDetail> {
        let call: Call = self.call_row(scope, call_id).await?.into();
        let as_of = call.timestamps.ended_at.unwrap_or_else(|| self.now());
        let participants = self.participants(scope, call_id).await?;
        let sessions = self.sessions(scope, call_id).await?;
        let intervals = self.intervals(scope, call_id).await?;
        let invitations = self.invitations(scope, call_id).await?;
        let transfers = self.transfers(scope, call_id).await?;

        let mut by_session: BTreeMap<_, Vec<ConnectionSpan>> = BTreeMap::new();
        for interval in &intervals {
            by_session
                .entry(interval.session_id)
                .or_default()
                .push(interval.span());
        }
        let mut by_participant: BTreeMap<CallParticipantId, Vec<ConnectionSpan>> = BTreeMap::new();
        let sessions: Vec<SessionDetail> = sessions
            .into_iter()
            .map(|session| {
                let spans = by_session.get(&session.id).cloned().unwrap_or_default();
                by_participant
                    .entry(session.participant_id)
                    .or_default()
                    .extend(spans.iter().copied());
                SessionDetail {
                    connected_duration_ms: millis(total_duration(&spans, as_of)),
                    session,
                }
            })
            .collect();
        let participants = participants
            .into_iter()
            .map(|participant| ParticipantDetail {
                connected_duration_ms: millis(union_duration(
                    by_participant
                        .get(&participant.id)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    as_of,
                )),
                participant,
            })
            .collect();
        let connected_duration_ms = call.timestamps.connected_at.map(|c| millis(as_of - c));
        Ok(CallDetail {
            call,
            participants,
            sessions,
            invitations,
            transfers,
            connected_duration_ms,
            as_of,
        })
    }

    /// Ring offers of a Call in `scope`, oldest first.
    pub async fn invitations(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallInvitation>> {
        self.call_row(scope, call_id).await?;
        Ok(call_invitations::Entity::find()
            .filter(call_invitations::Column::CallId.eq(call_id.as_uuid()))
            .order_by_asc(call_invitations::Column::IssuedAt)
            .order_by_asc(call_invitations::Column::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Transfers of a Call in `scope`, oldest first.
    pub async fn transfers(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallTransfer>> {
        self.call_row(scope, call_id).await?;
        Ok(call_transfers::Entity::find()
            .filter(call_transfers::Column::CallId.eq(call_id.as_uuid()))
            .order_by_asc(call_transfers::Column::InitiatedAt)
            .order_by_asc(call_transfers::Column::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// One page of Calls in `scope`, newest first (`created_at DESC, id DESC`, keyset).
    pub async fn list_calls(
        &self,
        scope: CallScope,
        query: ListCallsQuery,
    ) -> CallResult<CallPage> {
        let limit = match query.limit {
            0 => ListCallsQuery::DEFAULT_LIMIT,
            n if n > ListCallsQuery::MAX_LIMIT => {
                return Err(CallError::Validation("limit must be between 1 and 100"))
            }
            n => n,
        };
        let mut select = calls::Entity::find().filter(scoped(scope));
        if let Some(state) = query.state {
            select = select.filter(calls::Column::CurrentState.eq(state));
        }
        if let Some(after) = query.created_after {
            select = select.filter(calls::Column::CreatedAt.gte(after));
        }
        if let Some(before) = query.created_before {
            select = select.filter(calls::Column::CreatedAt.lt(before));
        }
        if let Some(initiator) = query.initiator_principal_id {
            select = select.filter(calls::Column::InitiatorPrincipalId.eq(initiator.as_uuid()));
        }
        if let Some(session) = query.privacy_relay_ref {
            select = select.filter(calls::Column::PrivacyRelayRef.eq(session));
        }
        if let Some(cursor) = &query.cursor {
            // (created_at, id) < (cursor.created_at, cursor.id)
            let row: SimpleExpr = Expr::tuple([
                Expr::col(calls::Column::CreatedAt).into(),
                Expr::col(calls::Column::Id).into(),
            ])
            .lt(Expr::tuple([
                Expr::value(cursor.created_at()),
                Expr::value(cursor.call_id().as_uuid()),
            ]));
            select = select.filter(row);
        }
        let mut rows = select
            .order_by_desc(calls::Column::CreatedAt)
            .order_by_desc(calls::Column::Id)
            .limit(limit + 1)
            .all(&self.db)
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|last| CallCursor::new(last.created_at, CallId::from_uuid(last.id)))
        } else {
            None
        };
        Ok(CallPage {
            calls: rows.into_iter().map(Into::into).collect(),
            next_cursor,
        })
    }

    /// Restricts a child-table query to Calls of `scope` (ownership columns are on every
    /// child row; this also proves the parent is in scope).
    fn child_scope<C: ColumnTrait>(
        scope: CallScope,
        call_id: CallId,
        tenant: C,
        application: C,
        environment: C,
        call: C,
    ) -> Condition {
        Condition::all()
            .add(tenant.eq(scope.tenant_id().as_uuid()))
            .add(application.eq(scope.application_id().as_uuid()))
            .add(environment.eq(scope.environment_id().as_uuid()))
            .add(call.eq(call_id.as_uuid()))
    }

    pub async fn participants(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallParticipant>> {
        use call_participants::Column as C;
        Ok(call_participants::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::CreatedAt)
            .order_by_asc(C::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn sessions(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallSession>> {
        use call_sessions::Column as C;
        Ok(call_sessions::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::CreatedAt)
            .order_by_asc(C::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn intervals(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallSessionInterval>> {
        use call_session_intervals::Column as C;
        Ok(call_session_intervals::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::SessionId)
            .order_by_asc(C::IntervalNumber)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Events of a Call in sequence order.
    pub async fn events(&self, scope: CallScope, call_id: CallId) -> CallResult<Vec<CallEvent>> {
        use call_events::Column as C;
        Ok(call_events::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::Sequence)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn authorizations(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallAuthorization>> {
        use call_authorizations::Column as C;
        Ok(call_authorizations::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::DecidedAt)
            .order_by_asc(C::CreatedAt)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn routes(&self, scope: CallScope, call_id: CallId) -> CallResult<Vec<CallRoute>> {
        use call_routes::Column as C;
        Ok(call_routes::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::SelectedAt)
            .order_by_asc(C::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn legs(&self, scope: CallScope, call_id: CallId) -> CallResult<Vec<CallLeg>> {
        use call_legs::Column as C;
        Ok(call_legs::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::CreatedAt)
            .order_by_asc(C::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn attempts(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallAttempt>> {
        use call_attempts::Column as C;
        Ok(call_attempts::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::AttemptNumber)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn quality_records(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallQualityRecord>> {
        use call_quality_records::Column as C;
        Ok(call_quality_records::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::CreatedAt)
            .order_by_asc(C::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn usage_records(
        &self,
        scope: CallScope,
        call_id: CallId,
    ) -> CallResult<Vec<CallUsageRecord>> {
        use call_usage_records::Column as C;
        Ok(call_usage_records::Entity::find()
            .filter(Self::child_scope(
                scope,
                call_id,
                C::TenantId,
                C::ApplicationId,
                C::EnvironmentId,
                C::CallId,
            ))
            .order_by_asc(C::CreatedAt)
            .order_by_asc(C::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Logical participation of one participant (union of its sessions' intervals) as
    /// of `as_of`, in milliseconds.
    pub async fn participant_connected_duration_ms(
        &self,
        scope: CallScope,
        call_id: CallId,
        participant_id: CallParticipantId,
        as_of: DateTime<Utc>,
    ) -> CallResult<i64> {
        let sessions: Vec<_> = self
            .sessions(scope, call_id)
            .await?
            .into_iter()
            .filter(|s| s.participant_id == participant_id)
            .map(|s| s.id)
            .collect();
        if sessions.is_empty()
            && !self
                .participants(scope, call_id)
                .await?
                .iter()
                .any(|p| p.id == participant_id)
        {
            return Err(CallError::NotFound);
        }
        let spans: Vec<_> = self
            .intervals(scope, call_id)
            .await?
            .iter()
            .filter(|i| sessions.contains(&i.session_id))
            .map(CallSessionInterval::span)
            .collect();
        Ok(millis(union_duration(&spans, as_of)))
    }

    /// Number of non-terminal Calls in `scope` (diagnostics / tests).
    pub async fn active_calls_in_scope(&self, scope: CallScope) -> CallResult<u64> {
        use sea_orm::PaginatorTrait;
        Ok(calls::Entity::find()
            .filter(scoped(scope))
            .filter(
                calls::Column::CurrentState
                    .is_not_in(CallState::TERMINAL.iter().map(|s| s.as_str())),
            )
            .count(&self.db)
            .await?)
    }
}
