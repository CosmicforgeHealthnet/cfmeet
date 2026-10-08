//! The loaded Communication Context aggregate and its safe public representation.
//!
//! The representation contains CF Meet ids, the owning application's own external
//! reference, versions, status, opaque subject references, channels, directions,
//! validity, policy and timestamps — never a telephone number, provider id, carrier,
//! Contact Vault material, service token or customer credential. ContactReference ids
//! are returned exactly as stored: no country, suffix or masked number is fabricated.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    directions::DirectionSpec,
    participants::{SubjectBody, SubjectRef},
    policy::{ContextPolicy, ContextPolicyBody},
    repository::{self, Lock},
    state::Window,
};
use crate::{
    error::{ErrorCode, RelayError, RelayResult},
    models::{ChannelRow, ContextRow, DirectionRow, ParticipantRow},
    scope::RelayScope,
    vocabulary::{CommunicationChannel, CommunicationContextStatus, DirectionPolicy},
};

pub fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

#[derive(Debug, Clone)]
pub struct ContextAggregate {
    pub row: ContextRow,
    /// Active participants.
    pub participants: Vec<ParticipantRow>,
    pub channels: Vec<ChannelRow>,
    /// Active directions.
    pub directions: Vec<DirectionRow>,
}

impl ContextAggregate {
    /// Loads the context of `scope` (NOT_FOUND for foreign / unknown ids).
    pub async fn load<C: ConnectionTrait>(
        db: &C,
        scope: RelayScope,
        id: Uuid,
        lock: Lock,
    ) -> RelayResult<Self> {
        let row = repository::context(db, scope, id, lock)
            .await?
            .ok_or(RelayError::Refused(ErrorCode::ContextNotFound))?;
        Self::load_children(db, row).await
    }

    pub async fn load_children<C: ConnectionTrait>(db: &C, row: ContextRow) -> RelayResult<Self> {
        Ok(Self {
            participants: repository::participants(db, row.id, true).await?,
            channels: repository::channels(db, row.id).await?,
            directions: repository::directions(db, row.id, true).await?,
            row,
        })
    }

    pub fn id(&self) -> Uuid {
        self.row.id
    }

    pub fn status(&self) -> RelayResult<CommunicationContextStatus> {
        self.row.status()
    }

    pub fn policy(&self) -> RelayResult<ContextPolicy> {
        self.row.policy()
    }

    pub fn window(&self) -> RelayResult<Window> {
        Ok(Window {
            status: self.status()?,
            starts_at: self.row.starts_at,
            expires_at: self.row.expires_at,
            grace_expires_at: self.row.grace_expires_at,
        })
    }

    pub fn enabled_channels(&self) -> RelayResult<Vec<CommunicationChannel>> {
        self.channels
            .iter()
            .filter(|c| c.enabled)
            .map(ChannelRow::channel)
            .collect()
    }

    /// Active subject -> participant id.
    pub fn subjects(&self) -> RelayResult<BTreeMap<SubjectRef, Uuid>> {
        self.participants
            .iter()
            .map(|p| Ok((SubjectRef::of_row(p)?, p.id)))
            .collect()
    }

    /// Participant id -> subject (active participants).
    pub fn subject_of(&self, participant_id: Uuid) -> Option<SubjectRef> {
        self.participants
            .iter()
            .find(|p| p.id == participant_id)
            .and_then(|p| SubjectRef::of_row(p).ok())
    }

    /// Active edges in subject terms.
    pub fn direction_specs(&self) -> RelayResult<Vec<(Uuid, DirectionSpec)>> {
        self.directions
            .iter()
            .filter_map(|d| {
                let from = self.subject_of(d.from_participant_id)?;
                let to = self.subject_of(d.to_participant_id)?;
                Some(
                    d.channel()
                        .map(|channel| (d.id, DirectionSpec { from, to, channel })),
                )
            })
            .collect()
    }

    /// Whether the context's direction model allows `from -> to` on `channel`
    /// (participant ids of this context; both must be active).
    pub fn allows_direction(
        &self,
        from: Uuid,
        to: Uuid,
        channel: CommunicationChannel,
    ) -> RelayResult<bool> {
        if from == to {
            return Ok(false);
        }
        let active = |id| self.participants.iter().any(|p| p.id == id);
        if !active(from) || !active(to) {
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

    /// Whether an explicit edge (`channel` None = every channel) is covered by the
    /// context: BIDIRECTIONAL covers every edge between active participants; EXPLICIT
    /// needs an active edge with the same channel or an all-channel edge.
    pub fn covers_edge(
        &self,
        from: Uuid,
        to: Uuid,
        channel: Option<CommunicationChannel>,
    ) -> RelayResult<bool> {
        if from == to {
            return Ok(false);
        }
        let active = |id| self.participants.iter().any(|p| p.id == id);
        if !active(from) || !active(to) {
            return Ok(false);
        }
        Ok(match self.row.direction_policy()? {
            DirectionPolicy::Bidirectional => true,
            DirectionPolicy::Explicit => self.directions.iter().any(|d| {
                d.from_participant_id == from && d.to_participant_id == to && {
                    let c = d.channel().ok().flatten();
                    c.is_none() || c == channel
                }
            }),
        })
    }

    /// Safe public representation.
    pub fn view(&self, now: DateTime<Utc>) -> RelayResult<Value> {
        let row = &self.row;
        let status = self.status()?;
        let effective = if !status.is_terminal() && self.window()?.finally_expired(now) {
            CommunicationContextStatus::Expired
        } else {
            status
        };
        let participants: Vec<Value> = self
            .participants
            .iter()
            .map(|p| {
                let subject = SubjectBody::from(SubjectRef::of_row(p)?);
                let mut v = json!({
                    "participantId": p.id,
                    "subjectKind": p.subject_kind,
                    "addedInVersion": p.added_in_version,
                    "addedAt": ts(p.added_at),
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
                    "directionId": d.id,
                    "fromParticipantId": d.from_participant_id,
                    "toParticipantId": d.to_participant_id,
                    "addedInVersion": d.added_in_version,
                });
                if let Some(c) = &d.channel {
                    v["channel"] = json!(c);
                }
                v
            })
            .collect();
        let mut v = json!({
            "contextId": row.id,
            "externalContextReference": row.external_context_reference,
            "version": row.version,
            "status": status.as_str(),
            "effectiveStatus": effective.as_str(),
            "directionPolicy": row.direction_policy,
            "participants": participants,
            "allowedChannels": self.enabled_channels()?.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "directions": directions,
            "allowedRelayStrategies": row.allowed_relay_strategies,
            "startsAt": ts(row.starts_at),
            "expiresAt": ts(row.expires_at),
            "policyVersion": row.policy_version,
            "policy": ContextPolicyBody::from(self.policy()?),
            "createdAt": ts(row.created_at),
            "updatedAt": ts(row.updated_at),
        });
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
