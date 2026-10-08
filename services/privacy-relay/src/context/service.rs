//! Communication Context commands and reads (Part D §27–§29).
//!
//! The consumer decides its business facts (who is assigned, whether the relationship is
//! complete) and tells CF Meet through these commands; once a command is ACCEPTED,
//! Privacy Relay owns the enforcement of the resulting authority. There is no callback to
//! the consumer on authorization: the context is the bounded authority snapshot.
//!
//! Every mutation requires `expectedVersion` (stale writes are 409
//! CONTEXT_VERSION_CONFLICT and can never restore removed participants, old validity,
//! old directions or channels, or revoked authority) and an Idempotency-Key. Lifecycle
//! changes are explicit commands; there is no status patch.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    aggregate::{ts, ContextAggregate},
    directions::{parse_channels, parse_directions, DirectionBody, DirectionSpec},
    participants::{parse_participant_set, SubjectBody, SubjectRef},
    policy::{ContextPolicy, ContextPolicyBody},
    repository::{self, Lock, NewContext},
    state::{validate_external_reference, validate_validity},
};
use crate::{
    audit::{self, AuditDraft},
    commands::{self, CommandKey, IdempotencyKey},
    db,
    enforcement::{self, NewJob},
    error::{ErrorCode, RelayError, RelayResult},
    models::{ContextRow, CONTEXT_COLUMNS},
    relay::session::{
        apply_diff, derive_for_context, diff, live_sessions_for_update, SessionAggregate,
    },
    scope::{environment_kind_str, CommandContext, RelayScope},
    service::{enforcement_body, CommandOutcome, RelayService},
    vocabulary::{
        AuditEventType, CommandOperation, CommunicationChannel, CommunicationContextStatus,
        DirectionPolicy, EnforcementReason, EnforcementState, PrivacyRelaySessionStatus,
        RelayStrategy,
    },
};

/// `POST /contexts` body. Scope, status, version, authorization results, telephone numbers,
/// providers and relay numbers are not inputs (unknown fields are refused).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateContextRequest {
    pub external_context_reference: String,
    pub participants: Vec<SubjectBody>,
    pub allowed_channels: Vec<CommunicationChannel>,
    pub direction_policy: DirectionPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directions: Vec<DirectionBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ContextPolicyBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_relay_strategies: Option<Vec<RelayStrategy>>,
}

/// Typed configuration update (never a status, never an arbitrary merge patch). Absent
/// members are unchanged; `participants` and `directions` replace the whole set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateContextRequest {
    pub expected_version: i64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ContextPolicyBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_relay_strategies: Option<Vec<RelayStrategy>>,
}

impl UpdateContextRequest {
    fn has_changes(&self) -> bool {
        self.participants.is_some()
            || self.allowed_channels.is_some()
            || self.direction_policy.is_some()
            || self.directions.is_some()
            || self.starts_at.is_some()
            || self.expires_at.is_some()
            || self.grace_expires_at.is_some()
            || self.policy.is_some()
            || self.allowed_relay_strategies.is_some()
    }
}

/// Body of an explicit lifecycle command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleRequest {
    pub expected_version: i64,
}

/// List filters (keyset pagination; no offsets).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub status: Option<CommunicationContextStatus>,
    #[serde(default)]
    pub external_context_reference: Option<String>,
}

pub const DEFAULT_PAGE: u32 = 50;
pub const MAX_PAGE: u32 = 100;

fn parse_strategies(strategies: Option<&Vec<RelayStrategy>>) -> RelayResult<Vec<RelayStrategy>> {
    match strategies {
        None => Ok(vec![RelayStrategy::AppOnly]),
        Some(list) => {
            let set: BTreeSet<_> = list.iter().copied().collect();
            if list.is_empty() || set.len() != list.len() {
                return Err(RelayError::Validation(
                    "allowedRelayStrategies must be a non-empty list without duplicates.",
                ));
            }
            Ok(set.into_iter().collect())
        }
    }
}

/// Opaque keyset cursor `(created_at, id)`.
pub fn encode_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    hex::encode(format!("{}|{}", created_at.timestamp_micros(), id))
}

pub fn decode_cursor(cursor: &str) -> RelayResult<(DateTime<Utc>, Uuid)> {
    const INVALID: RelayError = RelayError::Validation("cursor is invalid.");
    let raw = hex::decode(cursor).map_err(|_| INVALID)?;
    let text = String::from_utf8(raw).map_err(|_| INVALID)?;
    let (micros, id) = text.split_once('|').ok_or(INVALID)?;
    let at = micros
        .parse::<i64>()
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or(INVALID)?;
    Ok((at, Uuid::parse_str(id).map_err(|_| INVALID)?))
}

pub fn page_limit(limit: Option<u32>) -> RelayResult<u32> {
    match limit {
        None => Ok(DEFAULT_PAGE),
        Some(n) if (1..=MAX_PAGE).contains(&n) => Ok(n),
        Some(_) => Err(RelayError::Validation("limit must be between 1 and 100.")),
    }
}

impl RelayService {
    /// Creates a Communication Context at version 1 with its participants, channels,
    /// directions, audit event, outbox event and idempotency record — atomically.
    pub async fn create_context(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        req: CreateContextRequest,
    ) -> RelayResult<CommandOutcome> {
        let key = CommandKey::new(CommandOperation::CreateContext, key, None, &req)?;
        let now = self.now();
        let scope = cmd.scope();
        let txn = self.db.begin().await?;
        if let Some(prior) = commands::begin(&txn, cmd, &key).await? {
            txn.rollback().await?;
            return self.replay(prior.status, prior.body).await;
        }
        self.require_enabled()?;

        validate_external_reference(&req.external_context_reference)?;
        let participants = parse_participant_set(&req.participants, 2)?;
        require_contacts_in_scope(&txn, scope, participants.iter()).await?;
        let channels = parse_channels(&req.allowed_channels)?;
        let starts_at = req.starts_at.unwrap_or(now);
        validate_validity(starts_at, req.expires_at, req.grace_expires_at)?;
        if req.expires_at <= now {
            return Err(RelayError::Validation("expiresAt must be in the future."));
        }
        let strategies = parse_strategies(req.allowed_relay_strategies.as_ref())?;
        let edges = match req.direction_policy {
            DirectionPolicy::Bidirectional => {
                if !req.directions.is_empty() {
                    return Err(RelayError::Validation(
                        "directions are only accepted with directionPolicy EXPLICIT.",
                    ));
                }
                Vec::new()
            }
            DirectionPolicy::Explicit => {
                let edges = parse_directions(&req.directions, &participants, &channels)?;
                if edges.is_empty() {
                    return Err(RelayError::Validation(
                        "directionPolicy EXPLICIT requires at least one direction.",
                    ));
                }
                edges
            }
        };
        let policy: ContextPolicy = req.policy.map(Into::into).unwrap_or_default();

        if repository::context_by_reference(&txn, scope, &req.external_context_reference)
            .await?
            .is_some()
        {
            return Err(RelayError::Refused(ErrorCode::ContextReferenceConflict));
        }
        let id = Uuid::new_v4();
        repository::insert_context(
            &txn,
            scope,
            NewContext {
                id,
                environment_kind: environment_kind_str(cmd.environment_kind()),
                external_context_reference: &req.external_context_reference,
                direction_policy: req.direction_policy,
                strategies: &strategies,
                starts_at,
                expires_at: req.expires_at,
                grace_expires_at: req.grace_expires_at,
                policy,
                created_by_service_account_id: cmd.service_account_id(),
            },
            now,
        )
        .await
        .map_err(|e| match e.violated_unique() {
            Some("uq_communication_contexts_external_reference") => {
                RelayError::Refused(ErrorCode::ContextReferenceConflict)
            }
            _ => e,
        })?;
        let mut ids = BTreeMap::new();
        for subject in &participants {
            ids.insert(
                *subject,
                repository::insert_participant(&txn, scope, id, *subject, 1, now).await?,
            );
        }
        for channel in &channels {
            repository::set_channel(&txn, scope, id, *channel, true, 1, now).await?;
        }
        for edge in &edges {
            repository::insert_direction(
                &txn,
                scope,
                id,
                ids[&edge.from],
                ids[&edge.to],
                edge.channel,
                1,
                now,
            )
            .await?;
        }
        let ctx = ContextAggregate::load(&txn, scope, id, Lock::None).await?;
        audit::append(
            &txn,
            cmd,
            self.runtime,
            AuditDraft {
                event_type: AuditEventType::ContextCreated,
                context_id: id,
                context_version: 1,
                context_status: CommunicationContextStatus::Active.as_str(),
                session: None,
                session_status: None,
                enforcement_job_id: None,
                data: audit::data(vec![
                    (
                        "participants_added",
                        json!(ids.values().collect::<Vec<_>>()),
                    ),
                    (
                        "channels",
                        json!(channels.iter().map(|c| c.as_str()).collect::<Vec<_>>()),
                    ),
                    ("direction_policy", json!(req.direction_policy.as_str())),
                    ("directions_added", json!(edges.len())),
                    ("starts_at", json!(ts(starts_at))),
                    ("expires_at", json!(ts(req.expires_at))),
                ]),
            },
            now,
        )
        .await?;
        let body = json!({
            "context": ctx.view(now)?,
            "enforcement": enforcement_body(&[], EnforcementState::NotRequired),
        });
        commands::complete(&txn, cmd, &key, Some(id), Some(1), 201, &body, now).await?;
        txn.commit().await?;
        tracing::info!(context_id = %id, context_version = 1, "communication context created");
        Ok(CommandOutcome {
            status: 201,
            body,
            replayed: false,
        })
    }

    /// Typed, versioned configuration update (participant replacement, channels,
    /// directions, validity, policy, strategies). The context id never changes; the
    /// version increments exactly once; every live relay session is re-derived in the same
    /// transaction; removed participants lose authority the moment it commits.
    pub async fn update_context(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        context_id: Uuid,
        req: UpdateContextRequest,
    ) -> RelayResult<CommandOutcome> {
        let key = CommandKey::new(CommandOperation::UpdateContext, key, Some(context_id), &req)?;
        let now = self.now();
        let scope = cmd.scope();
        let txn = self.db.begin().await?;
        if let Some(prior) = commands::begin(&txn, cmd, &key).await? {
            txn.rollback().await?;
            return self.replay(prior.status, prior.body).await;
        }
        if !req.has_changes() {
            return Err(RelayError::Validation("At least one change is required."));
        }
        let ctx = ContextAggregate::load(&txn, scope, context_id, Lock::Update).await?;
        match ctx.status()? {
            CommunicationContextStatus::Revoked => return Err(ErrorCode::ContextRevoked.into()),
            CommunicationContextStatus::Expired => return Err(ErrorCode::ContextExpired.into()),
            _ => {}
        }
        if ctx.row.version != req.expected_version {
            return Err(ErrorCode::ContextVersionConflict.into());
        }
        if ctx.window()?.finally_expired(now) {
            return Err(ErrorCode::ContextExpired.into());
        }

        // Target configuration.
        let current: BTreeMap<SubjectRef, Uuid> = ctx.subjects()?;
        let target_subjects: Vec<SubjectRef> = match &req.participants {
            Some(bodies) => parse_participant_set(bodies, 1)?,
            None => current.keys().copied().collect(),
        };
        // Newly added ContactReferences must be enabled contacts of this scope.
        require_contacts_in_scope(
            &txn,
            scope,
            target_subjects.iter().filter(|s| !current.contains_key(s)),
        )
        .await?;
        let current_channels = ctx.enabled_channels()?;
        let channels = match &req.allowed_channels {
            Some(c) => parse_channels(c)?,
            None => current_channels.clone(),
        };
        let current_policy = ctx.row.direction_policy()?;
        let direction_policy = req.direction_policy.unwrap_or(current_policy);
        let strategies = match &req.allowed_relay_strategies {
            Some(_) => parse_strategies(req.allowed_relay_strategies.as_ref())?,
            None => ctx.row.strategies()?,
        };
        let starts_at = req.starts_at.unwrap_or(ctx.row.starts_at);
        let expires_at = req.expires_at.unwrap_or(ctx.row.expires_at);
        let grace_expires_at = req.grace_expires_at.or(ctx.row.grace_expires_at);
        validate_validity(starts_at, expires_at, grace_expires_at)?;
        let policy: ContextPolicy = match req.policy {
            Some(p) => p.into(),
            None => ctx.policy()?,
        };
        let current_edges: Vec<(Uuid, DirectionSpec)> = ctx.direction_specs()?;
        let target_edges: BTreeSet<DirectionSpec> = match direction_policy {
            DirectionPolicy::Bidirectional => {
                if req.directions.as_ref().is_some_and(|d| !d.is_empty()) {
                    return Err(RelayError::Validation(
                        "directions are only accepted with directionPolicy EXPLICIT.",
                    ));
                }
                BTreeSet::new()
            }
            DirectionPolicy::Explicit => {
                let edges: BTreeSet<DirectionSpec> = match &req.directions {
                    Some(bodies) => parse_directions(bodies, &target_subjects, &channels)?
                        .into_iter()
                        .collect(),
                    None => {
                        if current_policy == DirectionPolicy::Bidirectional {
                            return Err(RelayError::Validation(
                                "directions are required when directionPolicy becomes EXPLICIT.",
                            ));
                        }
                        current_edges
                            .iter()
                            .map(|(_, e)| *e)
                            .filter(|e| {
                                target_subjects.contains(&e.from)
                                    && target_subjects.contains(&e.to)
                                    && e.channel.is_none_or(|c| channels.contains(&c))
                            })
                            .collect()
                    }
                };
                if edges.is_empty() {
                    return Err(RelayError::Validation(
                        "directionPolicy EXPLICIT requires at least one direction.",
                    ));
                }
                edges
            }
        };

        // Diff.
        let target_set: BTreeSet<SubjectRef> = target_subjects.iter().copied().collect();
        let removed: Vec<(SubjectRef, Uuid)> = current
            .iter()
            .filter(|(s, _)| !target_set.contains(s))
            .map(|(s, id)| (*s, *id))
            .collect();
        let added: Vec<SubjectRef> = target_subjects
            .iter()
            .filter(|s| !current.contains_key(s))
            .copied()
            .collect();
        let edges_removed: Vec<Uuid> = current_edges
            .iter()
            .filter(|(_, e)| !target_edges.contains(e))
            .map(|(id, _)| *id)
            .collect();
        let current_edge_set: BTreeSet<DirectionSpec> =
            current_edges.iter().map(|(_, e)| *e).collect();
        let edges_added: Vec<DirectionSpec> = target_edges
            .difference(&current_edge_set)
            .copied()
            .collect();
        let channel_changes: Vec<(CommunicationChannel, bool)> = CommunicationChannel::ALL
            .iter()
            .filter_map(|c| {
                let want = channels.contains(c);
                (want != current_channels.contains(c)).then_some((*c, want))
            })
            .collect();
        let validity_changed = starts_at != ctx.row.starts_at
            || expires_at != ctx.row.expires_at
            || grace_expires_at != ctx.row.grace_expires_at;
        let policy_changed = policy != ctx.policy()?;
        let strategies_changed = strategies != ctx.row.strategies()?;
        let direction_policy_changed = direction_policy != current_policy;
        let unchanged = removed.is_empty()
            && added.is_empty()
            && edges_removed.is_empty()
            && edges_added.is_empty()
            && channel_changes.is_empty()
            && !validity_changed
            && !policy_changed
            && !strategies_changed
            && !direction_policy_changed;
        if unchanged {
            let body = json!({
                "context": ctx.view(now)?,
                "enforcement": enforcement_body(&[], EnforcementState::NotRequired),
            });
            commands::complete(
                &txn,
                cmd,
                &key,
                Some(context_id),
                Some(ctx.row.version),
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
        }

        // Apply (exactly one version increment).
        let version = ctx.row.version + 1;
        let updated = db::exec(
            &txn,
            "UPDATE communication_contexts SET version = $2, direction_policy = $3, allowed_relay_strategies = $4, \
               starts_at = $5, expires_at = $6, grace_expires_at = $7, policy_snapshot = $8, updated_at = $9 \
             WHERE id = $1 AND version = $10",
            vec![
                context_id.into(),
                version.into(),
                direction_policy.as_str().into(),
                repository::strategy_values(&strategies).into(),
                starts_at.into(),
                expires_at.into(),
                grace_expires_at.into(),
                policy.snapshot().into(),
                now.into(),
                ctx.row.version.into(),
            ],
        )
        .await?;
        if updated != 1 {
            return Err(ErrorCode::ContextVersionConflict.into());
        }
        for edge_id in &edges_removed {
            repository::remove_direction(&txn, *edge_id, version, now).await?;
        }
        for (_, participant_id) in &removed {
            repository::remove_participant(&txn, *participant_id, version, now).await?;
        }
        let mut ids: BTreeMap<SubjectRef, Uuid> = current
            .iter()
            .filter(|(s, _)| target_set.contains(s))
            .map(|(s, id)| (*s, *id))
            .collect();
        let mut added_ids = Vec::new();
        for subject in &added {
            let id =
                repository::insert_participant(&txn, scope, context_id, *subject, version, now)
                    .await?;
            ids.insert(*subject, id);
            added_ids.push(id);
        }
        for (channel, enabled) in &channel_changes {
            repository::set_channel(&txn, scope, context_id, *channel, *enabled, version, now)
                .await?;
        }
        for edge in &edges_added {
            repository::insert_direction(
                &txn,
                scope,
                context_id,
                ids[&edge.from],
                ids[&edge.to],
                edge.channel,
                version,
                now,
            )
            .await?;
        }
        let after = ContextAggregate::load(&txn, scope, context_id, Lock::None).await?;
        let mut jobs = self.sync_sessions_in(&txn, cmd, &after, now).await?;

        let participants_changed = !removed.is_empty() || !added.is_empty();
        let removed_ids: Vec<Uuid> = removed.iter().map(|(_, id)| *id).collect();
        audit::append(
            &txn,
            cmd,
            self.runtime,
            AuditDraft {
                event_type: if participants_changed {
                    AuditEventType::ContextParticipantsChanged
                } else {
                    AuditEventType::ContextUpdated
                },
                context_id,
                context_version: version,
                context_status: after.status()?.as_str(),
                session: None,
                session_status: None,
                enforcement_job_id: None,
                data: audit::data(vec![
                    ("previous_version", json!(ctx.row.version)),
                    ("version", json!(version)),
                    ("participants_added", json!(added_ids)),
                    ("participants_removed", json!(removed_ids)),
                    ("directions_added", json!(edges_added.len())),
                    ("directions_removed", json!(edges_removed.len())),
                    (
                        "channels_changed",
                        json!(channel_changes
                            .iter()
                            .map(|(c, e)| json!({ "channel": c.as_str(), "enabled": e }))
                            .collect::<Vec<_>>()),
                    ),
                    ("direction_policy_changed", json!(direction_policy_changed)),
                    ("validity_changed", json!(validity_changed)),
                    ("policy_changed", json!(policy_changed)),
                    ("strategies_changed", json!(strategies_changed)),
                ]),
            },
            now,
        )
        .await?;
        let removed_principals: Vec<Uuid> =
            removed.iter().filter_map(|(s, _)| s.principal()).collect();
        if !removed_principals.is_empty() && policy.terminate_active_calls_on_participant_removal {
            jobs.push(
                enforcement::request(
                    &txn,
                    cmd,
                    self.runtime,
                    NewJob {
                        context_id,
                        context_version: version,
                        context_status: after.status()?.as_str(),
                        session: None,
                        reason: EnforcementReason::ParticipantRemoved,
                        affected_principals: &removed_principals,
                        terminate_active_calls: true,
                    },
                    now,
                )
                .await?,
            );
        }
        let mut body = json!({
            "context": after.view(now)?,
            "enforcement": enforcement_body(&jobs, EnforcementState::Pending),
        });
        commands::complete(
            &txn,
            cmd,
            &key,
            Some(context_id),
            Some(version),
            200,
            &body,
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(
            context_id = %context_id,
            context_version = version,
            participants_added = added.len(),
            participants_removed = removed.len(),
            "communication context updated"
        );
        body["enforcement"] = enforcement_body(&jobs, self.deliver_all(&jobs).await);
        Ok(CommandOutcome {
            status: 200,
            body,
            replayed: false,
        })
    }

    /// Re-derives every live relay session of `ctx` (its new state, already written in
    /// `txn`). Returns enforcement event ids of sessions whose window became empty.
    pub(crate) async fn sync_sessions_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CommandContext,
        ctx: &ContextAggregate,
        now: DateTime<Utc>,
    ) -> RelayResult<Vec<Uuid>> {
        let mut jobs = Vec::new();
        let scope = cmd.scope();
        let ctx_final = ctx.window()?.final_expires_at();
        for row in live_sessions_for_update(txn, ctx.id()).await? {
            let session = SessionAggregate::load_children(txn, row).await?;
            let desired = derive_for_context(&session, ctx)?;
            let d = diff(Some(&session), &desired)?;
            let s = &session.row;
            let starts_at = s.starts_at.max(ctx.row.starts_at);
            let expires_at = s.expires_at.min(ctx.row.expires_at);
            let grace_expires_at = s.grace_expires_at.map(|g| g.min(ctx_final).max(expires_at));
            let empty = starts_at >= expires_at;
            let validity_changed = starts_at != s.starts_at
                || expires_at != s.expires_at
                || grace_expires_at != s.grace_expires_at;
            let policy_changed = s.policy_snapshot != ctx.row.policy_snapshot;
            let direction_changed = desired.direction_policy != s.direction_policy()?;
            let changed = !d.is_empty() || validity_changed || policy_changed || direction_changed;
            if !changed && s.source_context_version == ctx.row.version {
                continue;
            }
            let version = if changed { s.version + 1 } else { s.version };
            if changed {
                apply_diff(txn, scope, ctx.id(), s.id, version, ctx, &d, now).await?;
            }
            if empty {
                // The context no longer leaves this session any window: it expires (status
                // change only; validity is left as recorded).
                db::exec(
                    txn,
                    "UPDATE privacy_relay_sessions SET status = 'EXPIRED', expired_at = $2, version = $3, \
                       source_context_version = $4, direction_policy = $5, policy_snapshot = $6, updated_at = $2 \
                     WHERE id = $1",
                    vec![
                        s.id.into(),
                        now.into(),
                        (s.version + 1).into(),
                        ctx.row.version.into(),
                        desired.direction_policy.as_str().into(),
                        ctx.row.policy_snapshot.clone().into(),
                    ],
                )
                .await?;
                audit::append(
                    txn,
                    cmd,
                    self.runtime,
                    AuditDraft {
                        event_type: AuditEventType::RelaySessionExpired,
                        context_id: ctx.id(),
                        context_version: ctx.row.version,
                        context_status: ctx.status()?.as_str(),
                        session: Some((s.id, s.version + 1)),
                        session_status: Some(PrivacyRelaySessionStatus::Expired.as_str()),
                        enforcement_job_id: None,
                        data: audit::data(vec![
                            ("source_context_version", json!(ctx.row.version)),
                            ("reason", json!("CONTEXT_VALIDITY_NARROWED")),
                        ]),
                    },
                    now,
                )
                .await?;
                jobs.push(
                    enforcement::request(
                        txn,
                        cmd,
                        self.runtime,
                        NewJob {
                            context_id: ctx.id(),
                            context_version: ctx.row.version,
                            context_status: ctx.status()?.as_str(),
                            session: Some((s.id, s.version + 1)),
                            reason: EnforcementReason::SessionExpired,
                            affected_principals: &[],
                            terminate_active_calls: true,
                        },
                        now,
                    )
                    .await?,
                );
                continue;
            }
            db::exec(
                txn,
                "UPDATE privacy_relay_sessions SET source_context_version = $2, version = $3, direction_policy = $4, \
                   starts_at = $5, expires_at = $6, grace_expires_at = $7, policy_snapshot = $8, updated_at = $9 \
                 WHERE id = $1",
                vec![
                    s.id.into(),
                    ctx.row.version.into(),
                    version.into(),
                    desired.direction_policy.as_str().into(),
                    starts_at.into(),
                    expires_at.into(),
                    grace_expires_at.into(),
                    ctx.row.policy_snapshot.clone().into(),
                    now.into(),
                ],
            )
            .await?;
            if changed {
                let mut evidence = d.evidence();
                evidence["source_context_version"] = json!(ctx.row.version);
                evidence["validity_changed"] = json!(validity_changed);
                evidence["policy_changed"] = json!(policy_changed);
                evidence["direction_policy_changed"] = json!(direction_changed);
                audit::append(
                    txn,
                    cmd,
                    self.runtime,
                    AuditDraft {
                        event_type: AuditEventType::RelaySessionUpdated,
                        context_id: ctx.id(),
                        context_version: ctx.row.version,
                        context_status: ctx.status()?.as_str(),
                        session: Some((s.id, version)),
                        session_status: Some(s.status()?.as_str()),
                        enforcement_job_id: None,
                        data: evidence.as_object().cloned().unwrap_or_default(),
                    },
                    now,
                )
                .await?;
            }
        }
        Ok(jobs)
    }

    /// Moves every live session of `ctx` to `to` (context lifecycle cascade).
    pub(crate) async fn cascade_sessions_in(
        &self,
        txn: &DatabaseTransaction,
        cmd: &CommandContext,
        ctx: &ContextAggregate,
        to: PrivacyRelaySessionStatus,
        now: DateTime<Utc>,
    ) -> RelayResult<()> {
        let (event, stamp) = match to {
            PrivacyRelaySessionStatus::Revoked => {
                (AuditEventType::RelaySessionRevoked, "revoked_at")
            }
            PrivacyRelaySessionStatus::Completed => {
                (AuditEventType::RelaySessionCompleted, "completed_at")
            }
            PrivacyRelaySessionStatus::Expired => {
                (AuditEventType::RelaySessionExpired, "expired_at")
            }
            _ => return Err(ErrorCode::InternalError.into()),
        };
        for row in live_sessions_for_update(txn, ctx.id()).await? {
            let status = row.status()?;
            if status == to
                || (to == PrivacyRelaySessionStatus::Completed
                    && status == PrivacyRelaySessionStatus::Completed)
            {
                continue;
            }
            let version = row.version + 1;
            db::exec(
                txn,
                &format!(
                    "UPDATE privacy_relay_sessions SET status = $2, version = $3, {stamp} = $4, \
                       source_context_version = $5, updated_at = $4 WHERE id = $1"
                ),
                vec![
                    row.id.into(),
                    to.as_str().into(),
                    version.into(),
                    now.into(),
                    ctx.row.version.into(),
                ],
            )
            .await?;
            audit::append(
                txn,
                cmd,
                self.runtime,
                AuditDraft {
                    event_type: event,
                    context_id: ctx.id(),
                    context_version: ctx.row.version,
                    context_status: ctx.status()?.as_str(),
                    session: Some((row.id, version)),
                    session_status: Some(to.as_str()),
                    enforcement_job_id: None,
                    data: audit::data(vec![
                        ("previous_status", json!(status.as_str())),
                        ("cascade", json!("CONTEXT_LIFECYCLE")),
                    ]),
                },
                now,
            )
            .await?;
        }
        Ok(())
    }

    /// Shared skeleton of the explicit lifecycle commands.
    async fn lifecycle_command(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        context_id: Uuid,
        req: LifecycleRequest,
        operation: CommandOperation,
    ) -> RelayResult<CommandOutcome> {
        let key = CommandKey::new(operation, key, Some(context_id), &req)?;
        let now = self.now();
        let scope = cmd.scope();
        let txn = self.db.begin().await?;
        if let Some(prior) = commands::begin(&txn, cmd, &key).await? {
            txn.rollback().await?;
            return self.replay(prior.status, prior.body).await;
        }
        let ctx = ContextAggregate::load(&txn, scope, context_id, Lock::Update).await?;
        let status = ctx.status()?;
        let policy = ctx.policy()?;
        use CommunicationContextStatus as S;
        // (target status, audit event, session cascade, enforcement)
        let plan: Option<LifecyclePlan> = match operation {
            CommandOperation::RevokeContext => match status {
                S::Revoked => None,
                S::Expired => return Err(ErrorCode::ContextExpired.into()),
                _ => Some((
                    S::Revoked,
                    AuditEventType::ContextRevoked,
                    Some(PrivacyRelaySessionStatus::Revoked),
                    policy
                        .terminate_active_calls_on_revoke
                        .then_some((EnforcementReason::ContextRevoked, true)),
                )),
            },
            CommandOperation::SuspendContext => match status {
                S::Suspended => None,
                S::Active => Some((
                    S::Suspended,
                    AuditEventType::ContextSuspended,
                    None,
                    policy
                        .terminate_active_calls_on_suspend
                        .then_some((EnforcementReason::ContextSuspended, true)),
                )),
                S::Completed => return Err(ErrorCode::ContextNotActive.into()),
                S::Revoked => return Err(ErrorCode::ContextRevoked.into()),
                S::Expired => return Err(ErrorCode::ContextExpired.into()),
            },
            CommandOperation::ResumeContext => match status {
                S::Active => None,
                S::Suspended => {
                    self.require_enabled()?;
                    if now >= ctx.row.expires_at || ctx.window()?.finally_expired(now) {
                        return Err(ErrorCode::ContextExpired.into());
                    }
                    Some((S::Active, AuditEventType::ContextResumed, None, None))
                }
                S::Completed => return Err(ErrorCode::ContextNotActive.into()),
                S::Revoked => return Err(ErrorCode::ContextRevoked.into()),
                S::Expired => return Err(ErrorCode::ContextExpired.into()),
            },
            CommandOperation::CompleteContext => match status {
                S::Completed => None,
                S::Active | S::Suspended => {
                    let grace_remains = ctx.row.grace_expires_at.is_some_and(|g| now < g);
                    Some((
                        S::Completed,
                        AuditEventType::ContextCompleted,
                        Some(PrivacyRelaySessionStatus::Completed),
                        (!grace_remains).then_some((EnforcementReason::ContextCompleted, true)),
                    ))
                }
                S::Revoked => return Err(ErrorCode::ContextRevoked.into()),
                S::Expired => return Err(ErrorCode::ContextExpired.into()),
            },
            _ => return Err(ErrorCode::InternalError.into()),
        };
        let Some((target, event, cascade, enforce)) = plan else {
            // Already in the requested state: a safe idempotent answer (no new version, no
            // event), whatever expectedVersion says.
            let body = json!({
                "context": ctx.view(now)?,
                "enforcement": enforcement_body(&[], EnforcementState::NotRequired),
            });
            commands::complete(
                &txn,
                cmd,
                &key,
                Some(context_id),
                Some(ctx.row.version),
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
        if ctx.row.version != req.expected_version {
            return Err(ErrorCode::ContextVersionConflict.into());
        }
        let version = ctx.row.version + 1;
        let stamp = match target {
            S::Revoked => "revoked_at = $4,",
            S::Suspended => "suspended_at = $4,",
            S::Completed => "completed_at = $4,",
            S::Active => "suspended_at = NULL,",
            S::Expired => "expired_at = $4,",
        };
        let updated = db::exec(
            &txn,
            &format!(
                "UPDATE communication_contexts SET status = $2, version = $3, {stamp} updated_at = $4 \
                 WHERE id = $1 AND version = $5"
            ),
            vec![
                context_id.into(),
                target.as_str().into(),
                version.into(),
                now.into(),
                ctx.row.version.into(),
            ],
        )
        .await?;
        if updated != 1 {
            return Err(ErrorCode::ContextVersionConflict.into());
        }
        let after = ContextAggregate::load(&txn, scope, context_id, Lock::None).await?;
        if let Some(to) = cascade {
            self.cascade_sessions_in(&txn, cmd, &after, to, now).await?;
        } else {
            // Keep live sessions' source version current.
            db::exec(
                &txn,
                "UPDATE privacy_relay_sessions SET source_context_version = $2, updated_at = $3 \
                 WHERE context_id = $1 AND status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED')",
                vec![context_id.into(), version.into(), now.into()],
            )
            .await?;
        }
        audit::append(
            &txn,
            cmd,
            self.runtime,
            AuditDraft {
                event_type: event,
                context_id,
                context_version: version,
                context_status: target.as_str(),
                session: None,
                session_status: None,
                enforcement_job_id: None,
                data: audit::data(vec![
                    ("previous_status", json!(status.as_str())),
                    ("previous_version", json!(ctx.row.version)),
                ]),
            },
            now,
        )
        .await?;
        let mut jobs = Vec::new();
        if let Some((reason, terminate)) = enforce {
            jobs.push(
                enforcement::request(
                    &txn,
                    cmd,
                    self.runtime,
                    NewJob {
                        context_id,
                        context_version: version,
                        context_status: target.as_str(),
                        session: None,
                        reason,
                        affected_principals: &[],
                        terminate_active_calls: terminate,
                    },
                    now,
                )
                .await?,
            );
        }
        let mut body = json!({
            "context": after.view(now)?,
            "enforcement": enforcement_body(&jobs, EnforcementState::Pending),
        });
        commands::complete(
            &txn,
            cmd,
            &key,
            Some(context_id),
            Some(version),
            200,
            &body,
            now,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(context_id = %context_id, context_version = version, status = target.as_str(), "communication context lifecycle changed");
        body["enforcement"] = enforcement_body(&jobs, self.deliver_all(&jobs).await);
        Ok(CommandOutcome {
            status: 200,
            body,
            replayed: false,
        })
    }

    pub async fn suspend_context(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.lifecycle_command(cmd, key, id, req, CommandOperation::SuspendContext)
            .await
    }

    pub async fn resume_context(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.lifecycle_command(cmd, key, id, req, CommandOperation::ResumeContext)
            .await
    }

    pub async fn complete_context(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.lifecycle_command(cmd, key, id, req, CommandOperation::CompleteContext)
            .await
    }

    pub async fn revoke_context(
        &self,
        cmd: &CommandContext,
        key: IdempotencyKey,
        id: Uuid,
        req: LifecycleRequest,
    ) -> RelayResult<CommandOutcome> {
        self.lifecycle_command(cmd, key, id, req, CommandOperation::RevokeContext)
            .await
    }

    /// One context of `scope` (CONTEXT_NOT_FOUND for unknown and foreign ids alike).
    pub async fn get_context(&self, scope: RelayScope, id: Uuid) -> RelayResult<Value> {
        let ctx = ContextAggregate::load(&self.db, scope, id, Lock::None).await?;
        ctx.view(self.now())
    }

    /// Contexts of `scope`, newest first, keyset-paginated.
    pub async fn list_contexts(
        &self,
        scope: RelayScope,
        query: ContextQuery,
    ) -> RelayResult<Value> {
        let limit = page_limit(query.limit)?;
        let mut sql = format!(
            "SELECT {CONTEXT_COLUMNS} FROM communication_contexts \
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
        if let Some(reference) = &query.external_context_reference {
            validate_external_reference(reference)?;
            values.push(reference.clone().into());
            sql.push_str(&format!(
                " AND external_context_reference = ${}",
                values.len()
            ));
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
        let mut rows: Vec<ContextRow> = db::all(&self.db, &sql, values).await?;
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
                ContextAggregate::load_children(&self.db, row)
                    .await?
                    .view(now)?,
            );
        }
        Ok(json!({ "items": items, "nextCursor": next }))
    }

    /// Privacy-safe evidence for support and debugging: lifecycle, versions, participant
    /// and direction history, sessions, recent authorization decisions, audit events and
    /// enforcement status. No contact values exist to leak.
    pub async fn context_evidence(&self, scope: RelayScope, id: Uuid) -> RelayResult<Value> {
        let now = self.now();
        let ctx = ContextAggregate::load(&self.db, scope, id, Lock::None).await?;
        let history: Vec<Value> = repository::participants(&self.db, id, false)
            .await?
            .iter()
            .map(|p| {
                let mut v = json!({
                    "participantId": p.id,
                    "subjectKind": p.subject_kind,
                    "addedInVersion": p.added_in_version,
                    "addedAt": ts(p.added_at),
                });
                if let Some(r) = p.principal_ref {
                    v["principalId"] = json!(r);
                }
                if let Some(r) = p.contact_ref {
                    v["contactReferenceId"] = json!(r);
                }
                if let (Some(rv), Some(ra)) = (p.removed_in_version, p.removed_at) {
                    v["removedInVersion"] = json!(rv);
                    v["removedAt"] = json!(ts(ra));
                }
                v
            })
            .collect();
        let directions: Vec<Value> = repository::directions(&self.db, id, false)
            .await?
            .iter()
            .map(|d| {
                let mut v = json!({
                    "directionId": d.id,
                    "fromParticipantId": d.from_participant_id,
                    "toParticipantId": d.to_participant_id,
                    "addedInVersion": d.added_in_version,
                });
                if let Some(c) = &d.channel {
                    v["channel"] = json!(c);
                }
                if let Some(rv) = d.removed_in_version {
                    v["removedInVersion"] = json!(rv);
                }
                v
            })
            .collect();
        let mut sessions = Vec::new();
        for row in db::all::<crate::models::SessionRow, _>(
            &self.db,
            &format!(
                "SELECT {} FROM privacy_relay_sessions WHERE context_id = $1 ORDER BY created_at, id LIMIT 100",
                crate::models::SESSION_COLUMNS
            ),
            vec![id.into()],
        )
        .await?
        {
            sessions.push(SessionAggregate::load_children(&self.db, row).await?.view(now)?);
        }
        let decisions: Vec<Value> = db::all::<DecisionEvidence, _>(
            &self.db,
            "SELECT id, session_id, context_version, session_version, channel, route, purpose, decision, reason, \
               strategy, decided_at FROM relay_authorization_decisions WHERE context_id = $1 \
             ORDER BY decided_at DESC, id LIMIT 100",
            vec![id.into()],
        )
        .await?
        .into_iter()
        .map(|d| {
            json!({
                "decisionId": d.id,
                "relaySessionId": d.session_id,
                "contextVersion": d.context_version,
                "relaySessionVersion": d.session_version,
                "channel": d.channel,
                "route": d.route,
                "purpose": d.purpose,
                "decision": d.decision,
                "reason": d.reason,
                "strategy": d.strategy,
                "decidedAt": ts(d.decided_at),
            })
        })
        .collect();
        let events: Vec<Value> = db::all::<AuditEvidence, _>(
            &self.db,
            "SELECT sequence, event_type, context_version, session_id, session_version, actor_kind, data, occurred_at \
             FROM relay_audit_events WHERE context_id = $1 ORDER BY sequence DESC LIMIT 200",
            vec![id.into()],
        )
        .await?
        .into_iter()
        .map(|e| {
            let mut v = json!({
                "sequence": e.sequence,
                "eventType": e.event_type,
                "contextVersion": e.context_version,
                "actorKind": e.actor_kind,
                "data": e.data,
                "occurredAt": ts(e.occurred_at),
            });
            if let (Some(s), Some(sv)) = (e.session_id, e.session_version) {
                v["relaySessionId"] = json!(s);
                v["relaySessionVersion"] = json!(sv);
            }
            v
        })
        .collect();
        let enforcement: Vec<Value> = db::all::<crate::models::EnforcementJobRow, _>(
            &self.db,
            &format!(
                "SELECT {} FROM privacy_relay_enforcement_jobs WHERE context_id = $1 ORDER BY created_at, id LIMIT 100",
                crate::models::ENFORCEMENT_JOB_COLUMNS
            ),
            vec![id.into()],
        )
        .await?
        .into_iter()
        .map(|j| {
            let mut v = json!({
                "enforcementEventId": j.enforcement_event_id,
                "reason": j.reason,
                "status": j.status,
                "contextVersion": j.context_version,
                "terminateActiveCalls": j.terminate_active_calls,
                "affectedPrincipalCount": j.affected_principal_refs.len(),
                "attemptCount": j.attempt_count,
                "createdAt": ts(j.created_at),
            });
            if let Some(s) = j.relay_session_id {
                v["relaySessionId"] = json!(s);
            }
            if let Some(c) = &j.last_error_category {
                v["lastErrorCategory"] = json!(c);
            }
            if let Some(at) = j.completed_at {
                v["completedAt"] = json!(ts(at));
            }
            v
        })
        .collect();
        Ok(json!({
            "context": ctx.view(now)?,
            "participantHistory": history,
            "directionHistory": directions,
            "sessions": sessions,
            "decisions": decisions,
            "auditEvents": events,
            "enforcement": enforcement,
        }))
    }
}

/// (target status, audit event, session cascade, enforcement (reason, terminate)).
type LifecyclePlan = (
    CommunicationContextStatus,
    AuditEventType,
    Option<PrivacyRelaySessionStatus>,
    Option<(EnforcementReason, bool)>,
);

#[derive(Debug, sea_orm::FromQueryResult)]
struct DecisionEvidence {
    id: Uuid,
    session_id: Uuid,
    context_version: i64,
    session_version: i64,
    channel: String,
    route: String,
    purpose: String,
    decision: String,
    reason: String,
    strategy: String,
    decided_at: DateTime<Utc>,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct AuditEvidence {
    sequence: i64,
    event_type: String,
    context_version: i64,
    session_id: Option<Uuid>,
    session_version: Option<i64>,
    actor_kind: String,
    data: Value,
    occurred_at: DateTime<Utc>,
}

/// Every ContactReference participant must be an enabled Contact Vault contact of the
/// SAME scope (foreign and unknown references are the same CONTACT_NOT_FOUND). The
/// vault is never asked for the number.
async fn require_contacts_in_scope<'a, C: sea_orm::ConnectionTrait>(
    conn: &C,
    scope: crate::scope::RelayScope,
    subjects: impl Iterator<Item = &'a SubjectRef>,
) -> RelayResult<()> {
    for subject in subjects {
        if let SubjectRef::Contact(id) = subject {
            if !crate::contact_vault::repository::lock_enabled_in_scope(conn, scope, *id).await? {
                return Err(RelayError::Refused(ErrorCode::ContactNotFound));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let at = DateTime::from_timestamp_micros(1_930_212_000_123_456).unwrap();
        let id = Uuid::new_v4();
        assert_eq!(decode_cursor(&encode_cursor(at, id)).unwrap(), (at, id));
        for bad in ["", "zz", &hex::encode("x|y"), &hex::encode("1|not-a-uuid")] {
            assert!(decode_cursor(bad).is_err(), "{bad}");
        }
        assert!(page_limit(Some(0)).is_err());
        assert!(page_limit(Some(101)).is_err());
        assert_eq!(page_limit(None).unwrap(), DEFAULT_PAGE);
    }

    #[test]
    fn requests_refuse_scope_status_and_contact_fields() {
        let base = json!({
            "externalContextReference": "case-1",
            "participants": [{ "principalId": Uuid::new_v4() }, { "principalId": Uuid::new_v4() }],
            "allowedChannels": ["VOICE"],
            "directionPolicy": "BIDIRECTIONAL",
            "expiresAt": "2031-03-01T11:00:00Z",
        });
        assert!(serde_json::from_value::<CreateContextRequest>(base.clone()).is_ok());
        for (field, value) in [
            ("tenantId", json!(Uuid::new_v4())),
            ("applicationId", json!(Uuid::new_v4())),
            ("environmentId", json!(Uuid::new_v4())),
            ("status", json!("ACTIVE")),
            ("version", json!(1)),
            ("authorizationResult", json!("AUTHORIZED")),
            ("phoneNumber", json!("+2348012345678")),
            ("provider", json!("x")),
            ("relayNumber", json!("+2348012345678")),
        ] {
            let mut body = base.clone();
            body[field] = value;
            assert!(
                serde_json::from_value::<CreateContextRequest>(body).is_err(),
                "{field}"
            );
        }
        let mut blob = base.clone();
        blob["policy"] = json!({ "anything": { "deeply": { "nested": [1, 2, 3] } } });
        assert!(serde_json::from_value::<CreateContextRequest>(blob).is_err());
        assert!(serde_json::from_value::<UpdateContextRequest>(
            json!({ "expectedVersion": 1, "status": "REVOKED" })
        )
        .is_err());
    }
}
