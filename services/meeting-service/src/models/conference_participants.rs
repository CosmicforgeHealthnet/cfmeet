//! ConferenceParticipant — ONE logical person/entity within ONE conference.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{AdmissionState, IdentityType, Role, RoleSource};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "conference_participants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub identity_type: IdentityType,
    pub user_id: Option<Uuid>,
    pub external_ref: Option<String>,
    /// Opaque, server-issued guest identity (never the display name).
    pub guest_identity: Option<String>,
    #[serde(skip_serializing)]
    pub credential_hash: Option<String>,
    pub invitation_id: Option<Uuid>,
    pub legacy_participant_id: Option<Uuid>,
    pub display_name: String,
    pub role: Role,
    pub role_source: RoleSource,
    pub admission_state: AdmissionState,
    pub requested_at: DateTimeUtc,
    pub admitted_at: Option<DateTimeUtc>,
    pub admitted_by: Option<String>,
    pub denied_at: Option<DateTimeUtc>,
    pub denied_by: Option<String>,
    pub denial_reason_code: Option<String>,
    pub denial_message: Option<String>,
    pub denial_internal_note: Option<String>,
    pub removed_at: Option<DateTimeUtc>,
    pub removed_by: Option<String>,
    pub banned_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub version: i32,
    // Part F — authoritative media restrictions and collaboration state.
    pub audio_locked_at: Option<DateTimeUtc>,
    pub audio_locked_by: Option<String>,
    pub video_locked_at: Option<DateTimeUtc>,
    pub video_locked_by: Option<String>,
    pub screen_share_locked_at: Option<DateTimeUtc>,
    pub screen_share_locked_by: Option<String>,
    pub chat_restricted_at: Option<DateTimeUtc>,
    pub chat_restricted_by: Option<String>,
    pub hand_raised_at: Option<DateTimeUtc>,
    pub hand_lowered_at: Option<DateTimeUtc>,
    pub spotlighted_at: Option<DateTimeUtc>,
    pub spotlighted_by: Option<String>,
    /// Privileged host/co-host breakout visit (not an assignment).
    pub breakout_visit_room_id: Option<Uuid>,
    pub collab_version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::conference_sessions::Entity",
        from = "Column::ConferenceId",
        to = "super::conference_sessions::Column::Id"
    )]
    Conference,
    #[sea_orm(has_many = "super::participant_sessions::Entity")]
    Sessions,
}

impl Related<super::conference_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Conference.def()
    }
}

impl Related<super::participant_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sessions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
