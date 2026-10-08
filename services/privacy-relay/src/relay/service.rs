//! Privacy Relay Session commands and reads (Part D §30).
//!
//! The server derives a session's effective authority from its context. A client may ask
//! for a NARROWER session (fewer participants, channels, directions, a shorter window, a
//! strategy among the context's allowed strategies) but never a broader one: anything the
//! context does not grant is refused.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sea_orm::TransactionTrait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::session::{
    apply_diff, diff, inherited_edges, session_row, DesiredSnapshot, Edge, SessionAggregate,
};
use crate::{
    audit::{self, AuditDraft},
    commands::{self, CommandKey, IdempotencyKey},
    context::{
        aggregate::{ts, ContextAggregate},
        directions::DirectionBody,
        participants::SubjectBody,
        repository::Lock,
        service::{decode_cursor, encode_cursor, page_limit, LifecycleRequest},
        state::validate_validity,
    },
    db,
    enforcement::{self, NewJob},
    error::{ErrorCode, RelayError, RelayResult},
    models::{SessionRow, SESSION_COLUMNS},
    scope::{CommandContext, RelayScope},
    service::{enforcement_body, CommandOutcome, RelayService},
    vocabulary::{
        AuditEventType, CommandOperation, CommunicationChannel, CommunicationContextStatus,
        DirectionPolicy, EnforcementReason, EnforcementState, PrivacyRelaySessionStatus,
        RelayStrategy, SnapshotScope,
    },
};

/// `POST /contexts/{id}/sessions` body. Everything except the strategy is optional and
/// can only NARROW the context. A relay identity / telephone number is never an input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSessionRequest {
    pub relay_strategy: RelayStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<SubjectBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_channels: Option<Vec<CommunicationChannel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction_policy: Option<DirectionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directions: Option<Vec<DirectionBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub status: Option<PrivacyRelaySessionStatus>,
    #[serde(default)]
    pub context_id: Option<Uuid>,
}

impl RelayService {
    pub async fn create_session(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        context_id: Uuid,
        req: CreateSessionRequest,
    ) -> RelayResult<CommandOutcome> {
        let key = CommandKey::new(CommandOperation::CreateSession, key, Some(context_id), &req)?;
        let now = self.now();
        let scope = cmd.scope();
        let txn = self.db.begin().await?;
        if let Some(prior) = commands::begin(&txn, cmd, &key).await? {
            txn.rollback().await?;
            return self.replay(prior.status, prior.body).await;
        }
        self.require_enabled()?;
        // The context lock serializes session derivation with context updates.
        let ctx = ContextAggregate::load(&txn, scope, context_id, Lock::Update).await?;
        match ctx.status()? {
            CommunicationContextStatus::Revoked => return Err(ErrorCode::ContextRevoked.into()),
            CommunicationContextStatus::Expired => return Err(ErrorCode::ContextExpired.into()),
            CommunicationContextStatus::Suspended => return Err(ErrorCode::ContextSuspended.into()),
            CommunicationContextStatus::Completed => return Err(ErrorCode::ContextNotActive.into()),
            CommunicationContextStatus::Active => {}
        }
        if now >= ctx.row.expires_at {
            return Err(ErrorCode::ContextExpired.into());
        }
        if !ctx.row.strategies()?.contains(&req.relay_strategy) {
            return Err(ErrorCode::StrategyNotAllowed.into());
        }

        // Participants (context participant ids).
        let subjects = ctx.subjects()?;
        let (participant_scope, participants): (SnapshotScope, BTreeSet<Uuid>) =
            match &req.participants {
                None => (
                    SnapshotScope::Inherited,
                    subjects.values().copied().collect(),
                ),
                Some(bodies) => {
                    let mut set = BTreeSet::new();
                    for body in bodies {
                        let subject = body.subject()?;
                        let id = subjects
                            .get(&subject)
                            .ok_or(RelayError::Refused(ErrorCode::ParticipantNotAuthorized))?;
                        if !set.insert(*id) {
                            return Err(RelayError::Validation(
                                "participants must not contain duplicates.",
                            ));
                        }
                    }
                    (SnapshotScope::Narrowed, set)
                }
            };
        if participants.len() < 2 {
            return Err(RelayError::Validation(
                "A relay session needs at least two participants.",
            ));
        }
        // Channels.
        let ctx_channels: BTreeSet<CommunicationChannel> =
            ctx.enabled_channels()?.into_iter().collect();
        let (channel_scope, channels) = match &req.allowed_channels {
            None => (SnapshotScope::Inherited, ctx_channels.clone()),
            Some(list) => {
                let set: BTreeSet<_> = list.iter().copied().collect();
                if list.is_empty() || set.len() != list.len() {
                    return Err(RelayError::Validation(
                        "allowedChannels must be a non-empty list without duplicates.",
                    ));
                }
                if !set.is_subset(&ctx_channels) {
                    return Err(ErrorCode::ChannelNotAllowed.into());
                }
                (SnapshotScope::Narrowed, set)
            }
        };
        // Directions.
        let ctx_policy = ctx.row.direction_policy()?;
        let (direction_scope, direction_policy, edges): (
            SnapshotScope,
            DirectionPolicy,
            BTreeSet<Edge>,
        ) = match (&req.direction_policy, &req.directions) {
            (Some(DirectionPolicy::Bidirectional), Some(d)) if !d.is_empty() => {
                return Err(RelayError::Validation(
                    "directions are only accepted with directionPolicy EXPLICIT.",
                ))
            }
            (Some(DirectionPolicy::Bidirectional), _) => {
                if ctx_policy != DirectionPolicy::Bidirectional {
                    return Err(ErrorCode::DirectionNotAllowed.into());
                }
                (
                    SnapshotScope::Inherited,
                    DirectionPolicy::Bidirectional,
                    BTreeSet::new(),
                )
            }
            (None, None) => (
                SnapshotScope::Inherited,
                ctx_policy,
                inherited_edges(&ctx, &participants)?,
            ),
            (Some(DirectionPolicy::Explicit), None) => {
                if ctx_policy != DirectionPolicy::Explicit {
                    return Err(RelayError::Validation(
                            "directionPolicy EXPLICIT requires directions when the context is BIDIRECTIONAL.",
                        ));
                }
                (
                    SnapshotScope::Inherited,
                    ctx_policy,
                    inherited_edges(&ctx, &participants)?,
                )
            }
            (_, Some(bodies)) => {
                if bodies.is_empty() || bodies.len() > crate::context::directions::MAX_DIRECTIONS {
                    return Err(RelayError::Validation(
                        "directions must contain 1-256 edges.",
                    ));
                }
                let mut set = BTreeSet::new();
                for body in bodies {
                    let from = subjects
                        .get(&body.from.subject()?)
                        .ok_or(RelayError::Refused(ErrorCode::ParticipantNotAuthorized))?;
                    let to = subjects
                        .get(&body.to.subject()?)
                        .ok_or(RelayError::Refused(ErrorCode::ParticipantNotAuthorized))?;
                    if !participants.contains(from) || !participants.contains(to) {
                        return Err(ErrorCode::ParticipantNotAuthorized.into());
                    }
                    if let Some(c) = body.channel {
                        if !channels.contains(&c) {
                            return Err(ErrorCode::ChannelNotAllowed.into());
                        }
                    }
                    if !ctx.covers_edge(*from, *to, body.channel)? {
                        return Err(ErrorCode::DirectionNotAllowed.into());
                    }
                    if !set.insert((*from, *to, body.channel)) {
                        return Err(RelayError::Validation(
                            "directions must not contain duplicates.",
                        ));
                    }
                }
                (SnapshotScope::Narrowed, DirectionPolicy::Explicit, set)
            }
        };
        // Validity, contained by the context.
        let starts_at = req.starts_at.unwrap_or(ctx.row.starts_at);
        let expires_at = req.expires_at.unwrap_or(ctx.row.expires_at);
        let grace_expires_at = match req.grace_expires_at {
            Some(g) => Some(g),
            None if req.expires_at.is_none() => ctx.row.grace_expires_at,
            None => None,
        };
        validate_validity(starts_at, expires_at, grace_expires_at)?;
        let ctx_final = ctx.window()?.final_expires_at();
        if starts_at < ctx.row.starts_at
            || expires_at > ctx.row.expires_at
            || grace_expires_at.unwrap_or(expires_at) > ctx_final
        {
            return Err(ErrorCode::ValidityNotContained.into());
        }
        if expires_at <= now {
            return Err(RelayError::Validation("expiresAt must be in the future."));
        }

        let id = Uuid::new_v4();
        db::exec(
            &txn,
            "INSERT INTO privacy_relay_sessions (id, context_id, tenant_id, application_id, environment_id, \
               source_context_version, version, status, strategy, direction_policy, participant_scope, \
               channel_scope, direction_scope, starts_at, expires_at, grace_expires_at, policy_version, \
               policy_snapshot, created_by_service_account_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, 1, 'ACTIVE', $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $18)",
            vec![
                id.into(),
                context_id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                ctx.row.version.into(),
                req.relay_strategy.as_str().into(),
                direction_policy.as_str().into(),
                participant_scope.as_str().into(),
                channel_scope.as_str().into(),
                direction_scope.as_str().into(),
                starts_at.into(),
                expires_at.into(),
                grace_expires_at.into(),
                ctx.row.policy_version.clone().into(),
                ctx.row.policy_snapshot.clone().into(),
                cmd.service_account_id().into(),
                now.into(),
            ],
        )
        .await?;
        let desired = DesiredSnapshot {
            participants,
            channels,
            direction_policy,
            edges,
        };
        let d = diff(None, &desired)?;
        apply_diff(&txn, scope, context_id, id, 1, &ctx, &d, now).await?;
        let session = SessionAggregate::load(&txn, scope, id, Lock::None).await?;
        audit::append(
            &txn,
            cmd,
            self.runtime,
            AuditDraft {
                event_type: AuditEventType::RelaySessionCreated,
                context_id,
                context_version: ctx.row.version,
                context_status: CommunicationContextStatus::Active.as_str(),
                session: Some((id, 1)),
                session_status: Some(PrivacyRelaySessionStatus::Active.as_str()),
                enforcement_job_id: None,
                data: audit::data(vec![
                    ("strategy", json!(req.relay_strategy.as_str())),
                    ("participant_scope", json!(participant_scope.as_str())),
                    ("channel_scope", json!(channel_scope.as_str())),
                    ("direction_scope", json!(direction_scope.as_str())),
                    ("participants", json!(desired.participants.len())),
                    ("starts_at", json!(ts(starts_at))),
                    ("expires_at", json!(ts(expires_at))),
                ]),
            },
            now,
        )
        .await?;
        let body = json!({
            "session": session.view(now)?,
            "enforcement": enforcement_body(&[], EnforcementState::NotRequired),
        });
        commands::complete(&txn, cmd, &key, Some(id), Some(1), 201, &body, now).await?;
        txn.commit().await?;
        tracing::info!(context_id = %context_id, relay_session_id = %id, "privacy relay session created");
        Ok(CommandOutcome {
            status: 201,
            body,
            replayed: false,
        })
    }

    async fn session_lifecycle(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        session_id: Uuid,
        req: LifecycleRequest,
        operation: CommandOperation,
    ) -> RelayResult<CommandOutcome> {
        let key = CommandKey::new(operation, key, Some(session_id), &req)?;
        let now = self.now();
        let scope = cmd.scope();
        let txn = self.db.begin().await?;
        if let Some(prior) = commands::begin(&txn, cmd, &key).await? {
            txn.rollback().await?;
            return self.replay(prior.status, prior.body).await;
        }
        // Lock order: context, then session.
        let probe = session_row(&txn, scope, session_id, Lock::None)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::SessionNotFound))?;
        let ctx = ContextAggregate::load(&txn, scope, probe.context_id, Lock::Update).await?;
        let session = SessionAggregate::load(&txn, scope, session_id, Lock::Update).await?;
        let status = session.status()?;
        let policy = ctx.policy()?;
        use PrivacyRelaySessionStatus as S;
        let plan: Option<(S, AuditEventType, Option<EnforcementReason>)> = match operation {
            CommandOperation::RevokeSession => match status {
                S::Revoked => None,
                S::Expired => return Err(ErrorCode::SessionNotActive.into()),
                _ => Some((
                    S::Revoked,
                    AuditEventType::RelaySessionRevoked,
                    policy
                        .terminate_active_calls_on_revoke
                        .then_some(EnforcementReason::SessionRevoked),
                )),
            },
            CommandOperation::SuspendSession => match status {
                S::Suspended => None,
                S::Active => Some((
                    S::Suspended,
                    AuditEventType::RelaySessionSuspended,
                    policy
                        .terminate_active_calls_on_suspend
                        .then_some(EnforcementReason::SessionSuspended),
                )),
                _ => return Err(ErrorCode::SessionNotActive.into()),
            },
            CommandOperation::ResumeSession => match status {
                S::Active => None,
                S::Suspended => {
                    self.require_enabled()?;
                    match ctx.status()? {
                        CommunicationContextStatus::Active
                        | CommunicationContextStatus::Suspended => {}
                        CommunicationContextStatus::Revoked => {
                            return Err(ErrorCode::ContextRevoked.into())
                        }
                        CommunicationContextStatus::Expired => {
                            return Err(ErrorCode::ContextExpired.into())
                        }
                        CommunicationContextStatus::Completed => {
                            return Err(ErrorCode::ContextNotActive.into())
                        }
                    }
                    if now >= session.row.expires_at {
                        return Err(ErrorCode::SessionNotActive.into());
                    }
                    Some((S::Active, AuditEventType::RelaySessionResumed, None))
                }
                _ => return Err(ErrorCode::SessionNotActive.into()),
            },
            _ => return Err(ErrorCode::InternalError.into()),
        };
        let Some((target, event, enforce)) = plan else {
            let body = json!({
                "session": session.view(now)?,
                "enforcement": enforcement_body(&[], EnforcementState::NotRequired),
            });
            commands::complete(
                &txn,
                cmd,
                &key,
                Some(session_id),
                Some(session.row.version),
                200,
                &body,
                now,
            )
            .await?;
            txn.commit().await?;
            return Ok(CommandOutcome {
                status: 200,
                body,
                replayed: false,
            });
        };
        if session.row.version != req.expected_version {
            return Err(ErrorCode::SessionVersionConflict.into());
        }
        let version = session.row.version + 1;
        let stamp = match target {
            S::Revoked => "revoked_at = $4,",
            S::Suspended => "suspended_at = $4,",
            S::Active => "suspended_at = NULL,",
            _ => return Err(ErrorCode::InternalError.into()),
        };
        db::exec(
            &txn,
            &format!(
                "UPDATE privacy_relay_sessions SET status = $2, version = $3, {stamp} updated_at = $4 \
                 WHERE id = $1 AND version = $5"
            ),
            vec![
                session_id.into(),
                target.as_str().into(),
                version.into(),
                now.into(),
                session.row.version.into(),
            ],
        )
        .await?;
        audit::append(
            &txn,
            cmd,
            self.runtime,
            AuditDraft {
                event_type: event,
                context_id: ctx.id(),
                context_version: ctx.row.version,
                context_status: ctx.status()?.as_str(),
                session: Some((session_id, version)),
                session_status: Some(target.as_str()),
                enforcement_job_id: None,
                data: audit::data(vec![
                    ("previous_status", json!(status.as_str())),
                    ("previous_version", json!(session.row.version)),
                ]),
            },
            now,
        )
        .await?;
        let mut jobs = Vec::new();
        if let Some(reason) = enforce {
            jobs.push(
                enforcement::request(
                    &txn,
                    cmd,
                    self.runtime,
                    NewJob {
                        context_id: ctx.id(),
                        context_version: ctx.row.version,
                        context_status: ctx.status()?.as_str(),
                        session: Some((session_id, version)),
                        reason,
                        affected_principals: &[],
                        terminate_active_calls: true,
                    },
                    now,
                )
                .await?,
            );
        }
        let after = SessionAggregate::load(&txn, scope, session_id, Lock::None).await?;
        let mut body = json!({
            "session": after.view(now)?,
            "enforcement": enforcement_body(&jobs, EnforcementState::Pending),
        });
        commands::complete(
            &txn,
            cmd,
            &key,
            Some(session_id),
            Some(version),
            200,
            &body,
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(relay_session_id = %session_id, session_version = version, status = target.as_str(), "privacy relay session lifecycle changed");
        body["enforcement"] = enforcement_body(&jobs, self.deliver_all(&jobs).await);
        Ok(CommandOutcome {
            status: 200,
            body,
            replayed: false,
        })
    }

    pub async fn suspend_session(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.session_lifecycle(cmd, key, id, req, CommandOperation::SuspendSession)
            .await
    }

    pub async fn resume_session(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.session_lifecycle(cmd, key, id, req, CommandOperation::ResumeSession)
            .await
    }

    pub async fn revoke_session(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.session_lifecycle(cmd, key, id, req, CommandOperation::RevokeSession)
            .await
    }

    pub async fn get_session(&self, scope: RelayScope, id: Uuid) -> RelayResult<Value> {
        SessionAggregate::load(&self.db, scope, id, Lock::None)
            .await?
            .view(self.now())
    }

    pub async fn list_sessions(
        &self,
        scope: RelayScope,
        query: SessionQuery,
    ) -> RelayResult<Value> {
        let limit = page_limit(query.limit)?;
        let mut sql = format!(
            "SELECT {SESSION_COLUMNS} FROM privacy_relay_sessions \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        let mut values: Vec<sea_orm::Value> = vec![
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
        ];
        if let Some(status) = query.status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some(context_id) = query.context_id {
            values.push(context_id.into());
            sql.push_str(&format!(" AND context_id = ${}", values.len()));
        }
        if let Some(cursor) = &query.cursor {
            let (at, id) = decode_cursor(cursor)?;
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) < (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        values.push((limit as i64 + 1).into());
        sql.push_str(&format!(
            " ORDER BY created_at DESC, id DESC LIMIT ${}",
            values.len()
        ));
        let mut rows: Vec<SessionRow> = db::all(&self.db, &sql, values).await?;
        let next = if rows.len() > limit as usize {
            rows.truncate(limit as usize);
            rows.last().map(|r| encode_cursor(r.created_at, r.id))
        } else {
            None
        };
        let now = self.now();
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(
                SessionAggregate::load_children(&self.db, row)
                    .await?
                    .view(now)?,
            );
        }
        Ok(json!({ "items": items, "nextCursor": next }))
    }
}
