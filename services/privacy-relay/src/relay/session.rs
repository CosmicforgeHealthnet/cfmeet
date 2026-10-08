//! The Privacy Relay Session aggregate, its derived authorization snapshot and the
//! synchronization that keeps every live session inside its context's CURRENT authority.
//!
//! A session's effective authority is always the INTERSECTION of the context and the
//! session (both are evaluated at authorization time). In addition, every authority
//! change of a context rewrites the snapshot of each live session in the SAME transaction,
//! so a stale session can never bypass the context:
//!
//! * removed context participants are revoked from every session;
//! * INHERITED sessions follow the context (new participants, channels, directions);
//!   NARROWED sessions only ever lose authority (never gain any the context did not
//!   already grant them);
//! * validity is clamped into the context's window (a session whose window becomes empty
//!   EXPIRES);
//! * the bounded policy follows the context.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    context::{
        aggregate::{ts, ContextAggregate},
        participants::{SubjectBody, SubjectRef},
        policy::ContextPolicyBody,
        repository::Lock,
        state::Window,
    },
    db,
    error::{ErrorCode, RelayError, RelayResult},
    models::{
        SessionChannelRow, SessionDirectionRow, SessionParticipantRow, SessionRow, SESSION_COLUMNS,
        SESSION_DIRECTION_COLUMNS, SESSION_PARTICIPANT_COLUMNS,
    },
    scope::RelayScope,
    vocabulary::{
        CommunicationChannel, CommunicationContextStatus, CommunicationRoute, DirectionPolicy,
        PrivacyRelaySessionStatus, SnapshotScope,
    },
};

/// An edge of the snapshot: (from participant, to participant, channel or all).
pub type Edge = (Uuid, Uuid, Option<CommunicationChannel>);

#[derive(Debug, Clone)]
pub struct SessionAggregate {
    pub row: SessionRow,
    /// Active snapshot participants.
    pub participants: Vec<SessionParticipantRow>,
    pub channels: Vec<SessionChannelRow>,
    /// Active snapshot edges.
    pub directions: Vec<SessionDirectionRow>,
}

pub async fn session_row<C: ConnectionTrait>(
    db: &C,
    scope: RelayScope,
    id: Uuid,
    lock: Lock,
) -> RelayResult<Option<SessionRow>> {
    let clause = match lock {
        Lock::None => "",
        Lock::Share => " FOR SHARE",
        Lock::Update => " FOR UPDATE",
    };
    db::one(
        db,
        &format!(
            "SELECT {SESSION_COLUMNS} FROM privacy_relay_sessions \
             WHERE id = $1 AND tenant_id = $2 AND application_id = $3 AND environment_id = $4{clause}"
        ),
        vec![
            id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
        ],
    )
    .await
}

/// Live (non-terminal) sessions of a context, locked for update, oldest first.
pub async fn live_sessions_for_update(
    txn: &DatabaseTransaction,
    context_id: Uuid,
) -> RelayResult<Vec<SessionRow>> {
    db::all(
        txn,
        &format!(
            "SELECT {SESSION_COLUMNS} FROM privacy_relay_sessions \
             WHERE context_id = $1 AND status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED') \
             ORDER BY created_at, id FOR UPDATE"
        ),
        vec![context_id.into()],
    )
    .await
}

impl SessionAggregate {
    pub async fn load_children<C: ConnectionTrait>(db: &C, row: SessionRow) -> RelayResult<Self> {
        let participants = db::all(
            db,
            &format!(
                "SELECT {SESSION_PARTICIPANT_COLUMNS} FROM privacy_relay_session_participants \
                 WHERE session_id = $1 AND status = 'ACTIVE' ORDER BY added_at, id"
            ),
            vec![row.id.into()],
        )
        .await?;
        let channels = db::all(
            db,
            "SELECT channel, enabled FROM privacy_relay_session_channels WHERE session_id = $1 ORDER BY channel",
            vec![row.id.into()],
        )
        .await?;
        let directions = db::all(
            db,
            &format!(
                "SELECT {SESSION_DIRECTION_COLUMNS} FROM privacy_relay_session_directions \
                 WHERE session_id = $1 AND status = 'ACTIVE' ORDER BY created_at, id"
            ),
            vec![row.id.into()],
        )
        .await?;
        Ok(Self {
            row,
            participants,
            channels,
            directions,
        })
    }

    pub async fn load<C: ConnectionTrait>(
        db: &C,
        scope: RelayScope,
        id: Uuid,
        lock: Lock,
    ) -> RelayResult<Self> {
        let row = session_row(db, scope, id, lock)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::SessionNotFound))?;
        Self::load_children(db, row).await
    }

    pub fn status(&self) -> RelayResult<PrivacyRelaySessionStatus> {
        self.row.status()
    }

    pub fn window(&self) -> RelayResult<Window> {
        Ok(Window {
            status: lifecycle(self.status()?),
            starts_at: self.row.starts_at,
            expires_at: self.row.expires_at,
            grace_expires_at: self.row.grace_expires_at,
        })
    }

    pub fn participant_ids(&self) -> BTreeSet<Uuid> {
        self.participants
            .iter()
            .map(|p| p.context_participant_id)
            .collect()
    }

    pub fn enabled_channels(&self) -> RelayResult<BTreeSet<CommunicationChannel>> {
        self.channels
            .iter()
            .filter(|c| c.enabled)
            .map(|c| {
                CommunicationChannel::parse(&c.channel)
                    .ok_or(RelayError::Refused(ErrorCode::InternalError))
            })
            .collect()
    }

    pub fn edges(&self) -> RelayResult<BTreeSet<Edge>> {
        self.directions
            .iter()
            .map(|d| Ok((d.from_participant_id, d.to_participant_id, d.channel()?)))
            .collect()
    }

    /// Whether the session's direction model allows `from -> to` on `channel`.
    pub fn allows_direction(
        &self,
        from: Uuid,
        to: Uuid,
        channel: CommunicationChannel,
    ) -> RelayResult<bool> {
        if from == to {
            return Ok(false);
        }
        let ids = self.participant_ids();
        if !ids.contains(&from) || !ids.contains(&to) {
            return Ok(false);
        }
        Ok(match self.row.direction_policy()? {
            DirectionPolicy::Bidirectional => true,
            DirectionPolicy::Explicit => self.directions.iter().any(|d| {
                d.from_participant_id == from
                    && d.to_participant_id == to
                    && d.channel().ok().flatten().is_none_or(|c| c == channel)
            }),
        })
    }

    /// Safe public representation.
    pub fn view(&self, now: DateTime<Utc>) -> RelayResult<Value> {
        let row = &self.row;
        let status = self.status()?;
        let effective = if !status.is_terminal() && self.window()?.finally_expired(now) {
            PrivacyRelaySessionStatus::Expired
        } else {
            status
        };
        let participants: Vec<Value> = self
            .participants
            .iter()
            .map(|p| {
                let subject = SubjectBody::from(SubjectRef::of_session_row(p)?);
                let mut v = json!({
                    "participantId": p.context_participant_id,
                    "subjectKind": p.subject_kind,
                    "sourceContextVersion": p.source_context_version,
                });
                if let Some(id) = subject.principal_id {
                    v["principalId"] = json!(id);
                }
                if let Some(id) = subject.contact_reference_id {
                    v["contactReferenceId"] = json!(id);
                }
                Ok(v)
            })
            .collect::<RelayResult<_>>()?;
        let directions: Vec<Value> = self
            .directions
            .iter()
            .map(|d| {
                let mut v = json!({
                    "fromParticipantId": d.from_participant_id,
                    "toParticipantId": d.to_participant_id,
                });
                if let Some(c) = &d.channel {
                    v["channel"] = json!(c);
                }
                v
            })
            .collect();
        let strategy = row.strategy()?;
        let executable: Vec<&str> = CommunicationRoute::ALL
            .iter()
            .filter(|r| strategy.permits(**r) && r.is_executable())
            .map(|r| r.as_str())
            .collect();
        let mut v = json!({
            "relaySessionId": row.id,
            "contextId": row.context_id,
            "version": row.version,
            "sourceContextVersion": row.source_context_version,
            "status": status.as_str(),
            "effectiveStatus": effective.as_str(),
            "strategy": strategy.as_str(),
            "executableRoutes": executable,
            "directionPolicy": row.direction_policy,
            "participantScope": row.participant_scope,
            "channelScope": row.channel_scope,
            "directionScope": row.direction_scope,
            "participants": participants,
            "allowedChannels": self.enabled_channels()?.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "directions": directions,
            "startsAt": ts(row.starts_at),
            "expiresAt": ts(row.expires_at),
            "policyVersion": row.policy_version,
            "policy": ContextPolicyBody::from(row.policy()?),
            "createdAt": ts(row.created_at),
            "updatedAt": ts(row.updated_at),
        });
        if let Some(binding) = row.relay_identity_binding_ref {
            v["relayIdentityBindingRef"] = json!(binding);
        }
        for (key, value) in [
            ("graceExpiresAt", row.grace_expires_at),
            ("suspendedAt", row.suspended_at),
            ("completedAt", row.completed_at),
            ("revokedAt", row.revoked_at),
            ("expiredAt", row.expired_at),
        ] {
            if let Some(at) = value {
                v[key] = json!(ts(at));
            }
        }
        Ok(v)
    }
}

/// Session status in the shared lifecycle vocabulary.
pub fn lifecycle(status: PrivacyRelaySessionStatus) -> CommunicationContextStatus {
    match status {
        PrivacyRelaySessionStatus::Active => CommunicationContextStatus::Active,
        PrivacyRelaySessionStatus::Suspended => CommunicationContextStatus::Suspended,
        PrivacyRelaySessionStatus::Revoked => CommunicationContextStatus::Revoked,
        PrivacyRelaySessionStatus::Completed => CommunicationContextStatus::Completed,
        PrivacyRelaySessionStatus::Expired => CommunicationContextStatus::Expired,
    }
}

/// The session snapshot a session SHOULD have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredSnapshot {
    pub participants: BTreeSet<Uuid>,
    pub channels: BTreeSet<CommunicationChannel>,
    pub direction_policy: DirectionPolicy,
    pub edges: BTreeSet<Edge>,
}

/// What a live session's snapshot becomes under `ctx` (never broader than `ctx`).
pub fn derive_for_context(
    session: &SessionAggregate,
    ctx: &ContextAggregate,
) -> RelayResult<DesiredSnapshot> {
    let ctx_participants: BTreeSet<Uuid> = ctx.participants.iter().map(|p| p.id).collect();
    let participants: BTreeSet<Uuid> = match session.row.participant_scope()? {
        SnapshotScope::Inherited => ctx_participants.clone(),
        SnapshotScope::Narrowed => session
            .participant_ids()
            .intersection(&ctx_participants)
            .copied()
            .collect(),
    };
    let ctx_channels: BTreeSet<CommunicationChannel> =
        ctx.enabled_channels()?.into_iter().collect();
    let channels = match session.row.channel_scope()? {
        SnapshotScope::Inherited => ctx_channels,
        SnapshotScope::Narrowed => session
            .enabled_channels()?
            .intersection(&ctx_channels)
            .copied()
            .collect(),
    };
    let ctx_policy = ctx.row.direction_policy()?;
    let (direction_policy, edges) = match session.row.direction_scope()? {
        SnapshotScope::Inherited => (ctx_policy, inherited_edges(ctx, &participants)?),
        SnapshotScope::Narrowed => {
            let mut kept = BTreeSet::new();
            for (from, to, channel) in session.edges()? {
                if participants.contains(&from)
                    && participants.contains(&to)
                    && ctx.covers_edge(from, to, channel)?
                {
                    kept.insert((from, to, channel));
                }
            }
            (DirectionPolicy::Explicit, kept)
        }
    };
    Ok(DesiredSnapshot {
        participants,
        channels,
        direction_policy,
        edges,
    })
}

/// Context edges among `participants` (EXPLICIT contexts); none for BIDIRECTIONAL.
pub fn inherited_edges(
    ctx: &ContextAggregate,
    participants: &BTreeSet<Uuid>,
) -> RelayResult<BTreeSet<Edge>> {
    if ctx.row.direction_policy()? == DirectionPolicy::Bidirectional {
        return Ok(BTreeSet::new());
    }
    ctx.directions
        .iter()
        .filter(|d| {
            participants.contains(&d.from_participant_id)
                && participants.contains(&d.to_participant_id)
        })
        .map(|d| Ok((d.from_participant_id, d.to_participant_id, d.channel()?)))
        .collect()
}

/// Difference between a session's current snapshot and the desired one.
#[derive(Debug, Default, Clone)]
pub struct SnapshotDiff {
    pub participants_added: Vec<Uuid>,
    pub participants_revoked: Vec<Uuid>,
    pub channels_changed: Vec<(CommunicationChannel, bool)>,
    pub edges_added: Vec<Edge>,
    pub edges_revoked: Vec<Uuid>,
}

impl SnapshotDiff {
    pub fn is_empty(&self) -> bool {
        self.participants_added.is_empty()
            && self.participants_revoked.is_empty()
            && self.channels_changed.is_empty()
            && self.edges_added.is_empty()
            && self.edges_revoked.is_empty()
    }

    pub fn evidence(&self) -> Value {
        json!({
            "participants_added": self.participants_added,
            "participants_revoked": self.participants_revoked,
            "channels_changed": self.channels_changed.iter()
                .map(|(c, enabled)| json!({ "channel": c.as_str(), "enabled": enabled }))
                .collect::<Vec<_>>(),
            "directions_added": self.edges_added.len(),
            "directions_revoked": self.edges_revoked.len(),
        })
    }
}

pub fn diff(
    current: Option<&SessionAggregate>,
    desired: &DesiredSnapshot,
) -> RelayResult<SnapshotDiff> {
    let mut out = SnapshotDiff::default();
    let (have_participants, have_channels, have_edges) = match current {
        Some(s) => (s.participant_ids(), s.enabled_channels()?, s.edges()?),
        None => (BTreeSet::new(), BTreeSet::new(), BTreeSet::new()),
    };
    out.participants_added = desired
        .participants
        .difference(&have_participants)
        .copied()
        .collect();
    out.participants_revoked = have_participants
        .difference(&desired.participants)
        .copied()
        .collect();
    for channel in CommunicationChannel::ALL {
        let want = desired.channels.contains(channel);
        let have = have_channels.contains(channel);
        if want != have {
            out.channels_changed.push((*channel, want));
        }
    }
    out.edges_added = desired.edges.difference(&have_edges).copied().collect();
    if let Some(s) = current {
        for d in &s.directions {
            let edge = (d.from_participant_id, d.to_participant_id, d.channel()?);
            if !desired.edges.contains(&edge) {
                out.edges_revoked.push(d.id);
            }
        }
    }
    Ok(out)
}

/// Writes a snapshot diff for `session_id` (session version after the change: `version`).
#[allow(clippy::too_many_arguments)]
pub async fn apply_diff(
    txn: &DatabaseTransaction,
    scope: RelayScope,
    context_id: Uuid,
    session_id: Uuid,
    version: i64,
    ctx: &ContextAggregate,
    diff: &SnapshotDiff,
    now: DateTime<Utc>,
) -> RelayResult<()> {
    let by_id: BTreeMap<Uuid, SubjectRef> = ctx
        .participants
        .iter()
        .map(|p| Ok((p.id, SubjectRef::of_row(p)?)))
        .collect::<RelayResult<_>>()?;
    for revoked in &diff.participants_revoked {
        db::exec(
            txn,
            "UPDATE privacy_relay_session_participants SET status = 'REVOKED', revoked_at = $3, \
               revoked_in_session_version = $4 \
             WHERE session_id = $1 AND context_participant_id = $2 AND status = 'ACTIVE'",
            vec![
                session_id.into(),
                (*revoked).into(),
                now.into(),
                version.into(),
            ],
        )
        .await?;
    }
    for added in &diff.participants_added {
        let subject = by_id
            .get(added)
            .ok_or(RelayError::Refused(ErrorCode::ParticipantNotAuthorized))?;
        db::exec(
            txn,
            "INSERT INTO privacy_relay_session_participants (id, session_id, context_id, tenant_id, \
               application_id, environment_id, context_participant_id, subject_kind, principal_ref, \
               contact_ref, source_context_version, status, added_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'ACTIVE', $12)",
            vec![
                Uuid::new_v4().into(),
                session_id.into(),
                context_id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                (*added).into(),
                subject.kind().as_str().into(),
                subject.principal().into(),
                subject.contact().into(),
                ctx.row.version.into(),
                now.into(),
            ],
        )
        .await?;
    }
    for (channel, enabled) in &diff.channels_changed {
        db::exec(
            txn,
            "INSERT INTO privacy_relay_session_channels (session_id, context_id, tenant_id, application_id, \
               environment_id, channel, enabled, changed_in_session_version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
             ON CONFLICT (session_id, channel) DO UPDATE SET enabled = EXCLUDED.enabled, \
               changed_in_session_version = EXCLUDED.changed_in_session_version, updated_at = EXCLUDED.updated_at",
            vec![
                session_id.into(),
                context_id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                channel.as_str().into(),
                (*enabled).into(),
                version.into(),
                now.into(),
            ],
        )
        .await?;
    }
    for edge_id in &diff.edges_revoked {
        db::exec(
            txn,
            "UPDATE privacy_relay_session_directions SET status = 'REVOKED', revoked_at = $2 \
             WHERE id = $1 AND status = 'ACTIVE'",
            vec![(*edge_id).into(), now.into()],
        )
        .await?;
    }
    for (from, to, channel) in &diff.edges_added {
        db::exec(
            txn,
            "INSERT INTO privacy_relay_session_directions (id, session_id, context_id, tenant_id, \
               application_id, environment_id, from_participant_id, to_participant_id, channel, status, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ACTIVE', $10)",
            vec![
                Uuid::new_v4().into(),
                session_id.into(),
                context_id.into(),
                scope.tenant_id().into(),
                scope.application_id().into(),
                scope.environment_id().into(),
                (*from).into(),
                (*to).into(),
                channel.map(|c| c.as_str().to_string()).into(),
                now.into(),
            ],
        )
        .await?;
    }
    Ok(())
}
