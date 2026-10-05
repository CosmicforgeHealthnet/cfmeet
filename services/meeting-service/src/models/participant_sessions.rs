//! ParticipantSession — ONE device/network/media session of a logical participant.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::SessionState;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "participant_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_participant_id: Uuid,
    pub conference_id: Uuid,
    /// Server-generated, unique within the conference.
    pub media_identity: String,
    pub state: SessionState,
    pub established_at: DateTimeUtc,
    /// Set only when the RTC provider confirms the connection (Part E).
    pub connected_at: Option<DateTimeUtc>,
    pub disconnected_at: Option<DateTimeUtc>,
    pub expires_at: Option<DateTimeUtc>,
    pub end_reason: Option<String>,
    pub reconnect_of_session_id: Option<Uuid>,
    pub client_kind: Option<String>,
    pub legacy_participant_id: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    /// Part F: provider-evidenced active screen share (track ref is internal).
    pub screen_share_started_at: Option<DateTimeUtc>,
    #[serde(skip_serializing)]
    pub screen_share_track_ref: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::conference_participants::Entity",
        from = "Column::ConferenceParticipantId",
        to = "super::conference_participants::Column::Id"
    )]
    Participant,
}

impl Related<super::conference_participants::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Participant.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Usable for media right now: an active state AND not past its validity window.
    /// `expires_at` caps every session (session TTL, guest TTL, healthcare appointment
    /// window); an active-but-expired row is treated exactly like an ended session even
    /// before a sweep marks it EXPIRED.
    pub fn is_live_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.state.is_active() && self.expires_at.map(|e| e > now).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SessionState;
    use chrono::{Duration, Utc};

    fn session(state: SessionState, expires_in: Option<i64>) -> Model {
        let now = Utc::now();
        Model {
            id: uuid::Uuid::new_v4(),
            conference_participant_id: uuid::Uuid::new_v4(),
            conference_id: uuid::Uuid::new_v4(),
            media_identity: "ps_x".into(),
            state,
            established_at: now,
            connected_at: None,
            disconnected_at: None,
            expires_at: expires_in.map(|s| now + Duration::seconds(s)),
            end_reason: None,
            reconnect_of_session_id: None,
            client_kind: None,
            legacy_participant_id: None,
            created_at: now,
            updated_at: now,
            screen_share_started_at: None,
            screen_share_track_ref: None,
        }
    }

    #[test]
    fn expiry_overrides_an_active_state() {
        let now = Utc::now();
        assert!(session(SessionState::Connected, Some(60)).is_live_at(now));
        assert!(session(SessionState::Pending, None).is_live_at(now));
        assert!(!session(SessionState::Connected, Some(-1)).is_live_at(now));
        assert!(!session(SessionState::Pending, Some(-60)).is_live_at(now));
        assert!(!session(SessionState::Terminated, Some(60)).is_live_at(now));
    }
}
