//! Direction model.
//!
//! BIDIRECTIONAL: any two distinct active authorized participants may communicate in both
//! directions (subject to channel and validity). EXPLICIT: only directed edges authorize;
//! `A -> B` never implies `B -> A`. An edge may be restricted to one channel.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::participants::{SubjectBody, SubjectRef};
use crate::{
    error::{RelayError, RelayResult},
    vocabulary::CommunicationChannel,
};

pub const MAX_DIRECTIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionBody {
    pub from: SubjectBody,
    pub to: SubjectBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<CommunicationChannel>,
}

/// A directed edge in subject terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DirectionSpec {
    pub from: SubjectRef,
    pub to: SubjectRef,
    /// `None`: every allowed channel.
    pub channel: Option<CommunicationChannel>,
}

/// Parses an edge set: bounded, distinct endpoints, no duplicates, endpoints among
/// `participants`, channel among `channels`.
pub fn parse_directions(
    bodies: &[DirectionBody],
    participants: &[SubjectRef],
    channels: &[CommunicationChannel],
) -> RelayResult<Vec<DirectionSpec>> {
    if bodies.len() > MAX_DIRECTIONS {
        return Err(RelayError::Validation(
            "directions contains too many edges.",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(bodies.len());
    for body in bodies {
        let spec = DirectionSpec {
            from: body.from.subject()?,
            to: body.to.subject()?,
            channel: body.channel,
        };
        if spec.from == spec.to {
            return Err(RelayError::Validation(
                "A direction must connect two different participants.",
            ));
        }
        if !participants.contains(&spec.from) || !participants.contains(&spec.to) {
            return Err(RelayError::Validation(
                "A direction must connect participants of the context.",
            ));
        }
        if let Some(channel) = spec.channel {
            if !channels.contains(&channel) {
                return Err(RelayError::Validation(
                    "A direction channel must be an allowed channel.",
                ));
            }
        }
        if !seen.insert(spec) {
            return Err(RelayError::Validation(
                "directions must not contain duplicates.",
            ));
        }
        out.push(spec);
    }
    Ok(out)
}

/// Parses an allowed-channel set: non-empty, distinct.
pub fn parse_channels(channels: &[CommunicationChannel]) -> RelayResult<Vec<CommunicationChannel>> {
    if channels.is_empty() {
        return Err(RelayError::Validation("allowedChannels must not be empty."));
    }
    let set: BTreeSet<_> = channels.iter().copied().collect();
    if set.len() != channels.len() {
        return Err(RelayError::Validation(
            "allowedChannels must not contain duplicates.",
        ));
    }
    Ok(set.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn edges_are_validated() {
        let a = SubjectRef::Principal(Uuid::new_v4());
        let b = SubjectRef::Principal(Uuid::new_v4());
        let c = SubjectRef::Principal(Uuid::new_v4());
        let edge = |from, to, channel| DirectionBody {
            from: SubjectBody::from(from),
            to: SubjectBody::from(to),
            channel,
        };
        let voice = [CommunicationChannel::Voice];
        assert_eq!(
            parse_directions(&[edge(a, b, None)], &[a, b], &voice)
                .unwrap()
                .len(),
            1
        );
        assert!(parse_directions(&[edge(a, a, None)], &[a, b], &voice).is_err());
        assert!(parse_directions(&[edge(a, c, None)], &[a, b], &voice).is_err());
        assert!(parse_directions(&[edge(a, b, None), edge(a, b, None)], &[a, b], &voice).is_err());
        assert!(parse_directions(
            &[edge(a, b, Some(CommunicationChannel::Messaging))],
            &[a, b],
            &voice
        )
        .is_err());
        assert!(parse_channels(&[]).is_err());
        assert!(
            parse_channels(&[CommunicationChannel::Voice, CommunicationChannel::Voice]).is_err()
        );
    }
}
