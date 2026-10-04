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
