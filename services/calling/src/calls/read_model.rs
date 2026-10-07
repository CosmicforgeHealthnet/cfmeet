//! Safe public read model of Calls (contract `cfmeet.calling.http-api`, schemas `Call`,
//! `CallSummary`, `CallList`).
//!
//! Exposes the canonical, provider-independent view only: ids, kind, state, version,
//! initiator principal, lifecycle timestamps, normalized terminal reason / failure
//! category, participants, session summaries and durations. Never provider / carrier ids,
//! execution bindings, media identities, credentials, telephone numbers, protected contact
//! data or raw event payloads.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use super::{
    aggregate::Call,
    ids::CallId,
    invitations::{CallInvitation, CallTransfer},
    service::{CallDetail, CallPage, ParticipantDetail, SessionDetail},
};

/// Opaque keyset cursor: the `(created_at, id)` of the last Call of a page. It carries
/// position only — never scope; every page is filtered by the caller's own scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallCursor {
    created_at: DateTime<Utc>,
    call_id: CallId,
}

impl CallCursor {
    const PREFIX: &'static str = "c1";

    pub fn new(created_at: DateTime<Utc>, call_id: CallId) -> Self {
        Self {
            created_at,
            call_id,
        }
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn call_id(&self) -> CallId {
        self.call_id
    }

    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(24);
        bytes.extend_from_slice(&self.created_at.timestamp_micros().to_be_bytes());
        bytes.extend_from_slice(self.call_id.as_uuid().as_bytes());
        format!("{}{}", Self::PREFIX, hex::encode(bytes))
    }

    pub fn decode(value: &str) -> Option<Self> {
        let hex = value.strip_prefix(Self::PREFIX)?;
        if hex.len() != 48 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let bytes = hex::decode(hex).ok()?;
        let micros = i64::from_be_bytes(bytes[..8].try_into().ok()?);
        let created_at = DateTime::<Utc>::from_timestamp_micros(micros)?;
        let id = uuid::Uuid::from_slice(&bytes[8..]).ok()?;
        Some(Self::new(created_at, CallId::from_uuid(id)))
    }
}

fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn ts_opt(at: Option<DateTime<Utc>>) -> Option<String> {
    at.map(ts)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSummaryView {
    pub call_id: String,
    pub kind: &'static str,
    pub state: &'static str,
    pub version: i64,
    pub initiator_principal_id: String,
    pub terminal_reason: Option<&'static str>,
    pub failure_category: Option<&'static str>,
    pub requested_at: String,
    pub authorized_at: Option<String>,
    pub ringing_at: Option<String>,
    pub answered_at: Option<String>,
    pub connected_at: Option<String>,
    pub ending_at: Option<String>,
    pub ended_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&Call> for CallSummaryView {
    fn from(c: &Call) -> Self {
        let t = &c.timestamps;
        Self {
            call_id: c.id.to_string(),
            kind: c.kind.as_str(),
            state: c.state.as_str(),
            version: c.version,
            initiator_principal_id: c.initiator_principal_id.to_string(),
            terminal_reason: c.terminal_reason.map(|r| r.as_str()),
            failure_category: c.failure_category.map(|f| f.as_str()),
            requested_at: ts(t.requested_at),
            authorized_at: ts_opt(t.authorized_at),
            ringing_at: ts_opt(t.ringing_at),
            answered_at: ts_opt(t.answered_at),
            connected_at: ts_opt(t.connected_at),
            ending_at: ts_opt(t.ending_at),
            ended_at: ts_opt(t.ended_at),
            created_at: ts(c.created_at),
            updated_at: ts(c.updated_at),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantView {
    pub participant_id: String,
    pub principal_id: String,
    pub principal_kind: &'static str,
    pub role: &'static str,
    pub display_name: Option<String>,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub connected_duration_ms: i64,
}

impl From<&ParticipantDetail> for ParticipantView {
    fn from(d: &ParticipantDetail) -> Self {
        let p = &d.participant;
        Self {
            participant_id: p.id.to_string(),
            principal_id: p.principal_id.to_string(),
            principal_kind: p.principal_kind.as_str(),
            role: p.role.as_str(),
            display_name: p.display_name.as_ref().map(|n| n.as_str().to_string()),
            created_at: ts(p.created_at),
            ended_at: ts_opt(p.ended_at),
            connected_duration_ms: d.connected_duration_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub session_id: String,
    pub participant_id: String,
    pub endpoint_category: &'static str,
    pub state: &'static str,
    pub connection_count: i32,
    pub created_at: String,
    pub first_connected_at: Option<String>,
    pub last_disconnected_at: Option<String>,
    pub ended_at: Option<String>,
    pub connected_duration_ms: i64,
    pub endpoint_id: Option<String>,
    pub media_access: &'static str,
    pub media_admitted_at: Option<String>,
    pub removed_at: Option<String>,
}

impl From<&SessionDetail> for SessionView {
    fn from(d: &SessionDetail) -> Self {
        let s = &d.session;
        Self {
            session_id: s.id.to_string(),
            participant_id: s.participant_id.to_string(),
            endpoint_category: s.endpoint_category.as_str(),
            state: s.state.as_str(),
            connection_count: s.generation,
            created_at: ts(s.created_at),
            first_connected_at: ts_opt(s.first_connected_at),
            last_disconnected_at: ts_opt(s.last_disconnected_at),
            ended_at: ts_opt(s.ended_at),
            connected_duration_ms: d.connected_duration_ms,
            endpoint_id: s.endpoint_id.map(|e| e.to_string()),
            media_access: s.media_access.as_str(),
            media_admitted_at: ts_opt(s.media_admitted_at),
            removed_at: ts_opt(s.removed_at),
        }
    }
}

/// A ring offer as the public API shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationView {
    pub invitation_id: String,
    pub participant_id: String,
    pub endpoint_id: String,
    pub purpose: &'static str,
    pub status: &'static str,
    pub invitation_generation: i32,
    pub issued_at: String,
    pub expires_at: String,
    pub finished_at: Option<String>,
    pub terminal_reason: Option<&'static str>,
    pub session_id: Option<String>,
}

impl From<&CallInvitation> for InvitationView {
    fn from(i: &CallInvitation) -> Self {
        Self {
            invitation_id: i.id.to_string(),
            participant_id: i.participant_id.to_string(),
            endpoint_id: i.endpoint_id.to_string(),
            purpose: i.purpose.as_str(),
            status: i.status.as_str(),
            invitation_generation: i.invitation_generation,
            issued_at: ts(i.issued_at),
            expires_at: ts(i.expires_at),
            finished_at: ts_opt(i.finished_at),
            terminal_reason: i.terminal_reason.map(|r| r.as_str()),
            session_id: i.session_id.map(|s| s.to_string()),
        }
    }
}

/// An application transfer as the public API shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub transfer_id: String,
    pub transferring_participant_id: String,
    pub target_principal_id: String,
    pub target_participant_id: Option<String>,
    pub state: &'static str,
    pub pre_transfer_state: &'static str,
    pub failure_reason: Option<&'static str>,
    pub failure_category: Option<&'static str>,
    pub initiated_at: String,
    pub completed_at: Option<String>,
    pub failed_at: Option<String>,
    pub cancelled_at: Option<String>,
}

impl From<&CallTransfer> for TransferView {
    fn from(t: &CallTransfer) -> Self {
        Self {
            transfer_id: t.id.to_string(),
            transferring_participant_id: t.transferring_participant_id.to_string(),
            target_principal_id: t.target_principal_id.to_string(),
            target_participant_id: t.target_participant_id.map(|p| p.to_string()),
            state: t.state.as_str(),
            pre_transfer_state: t.pre_transfer_state.as_str(),
            failure_reason: t.failure_reason.map(|r| r.as_str()),
            failure_category: t.failure_category.map(|c| c.as_str()),
            initiated_at: ts(t.initiated_at),
            completed_at: ts_opt(t.completed_at),
            failed_at: ts_opt(t.failed_at),
            cancelled_at: ts_opt(t.cancelled_at),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallView {
    #[serde(flatten)]
    pub summary: CallSummaryView,
    pub connected_duration_ms: Option<i64>,
    pub durations_as_of: String,
    pub participants: Vec<ParticipantView>,
    pub sessions: Vec<SessionView>,
    pub invitations: Vec<InvitationView>,
    pub transfers: Vec<TransferView>,
}

impl From<&CallDetail> for CallView {
    fn from(d: &CallDetail) -> Self {
        Self {
            summary: (&d.call).into(),
            connected_duration_ms: d.connected_duration_ms,
            durations_as_of: ts(d.as_of),
            participants: d.participants.iter().map(Into::into).collect(),
            sessions: d.sessions.iter().map(Into::into).collect(),
            invitations: d.invitations.iter().map(Into::into).collect(),
            transfers: d.transfers.iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallListView {
    pub data: Vec<CallSummaryView>,
    pub next_cursor: Option<String>,
}

impl From<&CallPage> for CallListView {
    fn from(p: &CallPage) -> Self {
        Self {
            data: p.calls.iter().map(Into::into).collect(),
            next_cursor: p.next_cursor.map(|c| c.encode()),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_tampering() {
        let at = Utc.with_ymd_and_hms(2031, 3, 1, 10, 0, 30).unwrap()
            + chrono::Duration::microseconds(123_456);
        let cursor = CallCursor::new(at, CallId::generate());
        let encoded = cursor.encode();
        assert_eq!(CallCursor::decode(&encoded), Some(cursor));
        for bad in [
            "",
            "c1",
            "c2".to_string().as_str(),
            &encoded[..encoded.len() - 1],
            &encoded.to_uppercase(),
            &format!("{encoded}00"),
            "c1zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ] {
            assert_eq!(CallCursor::decode(bad), None, "{bad}");
        }
    }
}
