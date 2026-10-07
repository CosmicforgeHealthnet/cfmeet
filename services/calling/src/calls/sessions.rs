//! Call sessions and their connection intervals (Part C §19).
//!
//! A session is one device / network participation of one participant, with a
//! server-generated media / signaling identity. A participant may own several sessions at
//! once (multi-device). Reconnect is explicit:
//!
//! * **renewal** — the same session reconnects ([`CallService::reconnect_session`]): a new
//!   connection interval opens on the same session;
//! * **replacement** — the connection / device identity genuinely changed
//!   ([`CallService::replace_session`]): the old session ends and a new session (new media
//!   identity) is created for the *same* participant, linked by `replaces_session_id`.
//!
//! Neither creates a second logical participant, and the Call is unchanged.
//!
//! [`CallService::reconnect_session`]: super::service::CallService::reconnect_session
//! [`CallService::replace_session`]: super::service::CallService::replace_session

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    duration::ConnectionSpan,
    ids::{CallId, CallParticipantId, CallSessionId, CallSessionIntervalId, MediaIdentity},
    kinds::{DisconnectReason, EndpointCategory, SessionState},
};
use crate::models::{call_session_intervals, call_sessions};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallSession {
    pub id: CallSessionId,
    pub call_id: CallId,
    pub participant_id: CallParticipantId,
    /// Server-generated signaling identity. Not exposed by the public read API.
    #[serde(skip)]
    pub media_identity: MediaIdentity,
    pub endpoint_category: EndpointCategory,
    pub state: SessionState,
    /// Connection intervals opened so far; `generation - 1` reconnects.
    pub generation: i32,
    pub replaces_session_id: Option<CallSessionId>,
    pub created_at: DateTime<Utc>,
    pub first_connected_at: Option<DateTime<Utc>>,
    pub last_disconnected_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl From<call_sessions::Model> for CallSession {
    fn from(m: call_sessions::Model) -> Self {
        Self {
            id: CallSessionId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            participant_id: CallParticipantId::from_uuid(m.participant_id),
            media_identity: MediaIdentity::from_persisted(m.media_identity),
            endpoint_category: m.endpoint_category,
            state: m.state,
            generation: m.generation,
            replaces_session_id: m.replaces_session_id.map(CallSessionId::from_uuid),
            created_at: m.created_at,
            first_connected_at: m.first_connected_at,
            last_disconnected_at: m.last_disconnected_at,
            ended_at: m.ended_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallSessionInterval {
    pub id: CallSessionIntervalId,
    pub call_id: CallId,
    pub session_id: CallSessionId,
    pub interval_number: i32,
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub disconnect_reason: Option<DisconnectReason>,
}

impl CallSessionInterval {
    pub fn span(&self) -> ConnectionSpan {
        ConnectionSpan {
            connected_at: self.connected_at,
            disconnected_at: self.disconnected_at,
        }
    }
}

impl From<call_session_intervals::Model> for CallSessionInterval {
    fn from(m: call_session_intervals::Model) -> Self {
        Self {
            id: CallSessionIntervalId::from_uuid(m.id),
            call_id: CallId::from_uuid(m.call_id),
            session_id: CallSessionId::from_uuid(m.session_id),
            interval_number: m.interval_number,
            connected_at: m.connected_at,
            disconnected_at: m.disconnected_at,
            disconnect_reason: m.disconnect_reason,
        }
    }
}
