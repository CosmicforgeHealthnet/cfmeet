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
