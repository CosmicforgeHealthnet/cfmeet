//! MeetingSpace — the persistent Phase 2 meeting definition (not a live call).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{
    AccessPolicy, MediaCapturePolicy, MeetingType, RecordOrigin, Role, SecurityProfile,
    SpaceStatus, WaitingRoomPolicy,
};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_spaces")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub legacy_meeting_id: Option<Uuid>,
    pub organization_id: Option<Uuid>,
    /// The CF Meet account owning the space; `None` exactly when the space is owned by a
    /// Platform Access application environment (`platform_*`).
    pub owner_user_id: Option<Uuid>,
    pub meeting_code: String,
    pub title: String,
    pub description: Option<String>,
    pub meeting_type: MeetingType,
    pub status: SpaceStatus,
    pub timezone: String,
    pub access_policy: AccessPolicy,
    pub waiting_room_policy: WaitingRoomPolicy,
    pub security_profile: SecurityProfile,
    pub default_role: Role,
    #[serde(skip_serializing)]
    pub passcode_hash: Option<String>,
    pub interactive_capacity: i32,
    pub viewer_capacity: i32,
    pub recording_policy: MediaCapturePolicy,
    pub transcription_policy: MediaCapturePolicy,
    pub retention_policy_ref: Option<String>,
    pub access_config: Json,
    pub role_policy: Json,
    pub schedule_policy: Json,
    pub origin: RecordOrigin,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub archived_at: Option<DateTimeUtc>,
    pub version: i32,
    /// Part F: sparse space-level collaboration policy defaults.
    pub collaboration_policy: Json,
    /// Part H: sparse space-level profile policy overrides.
    pub profile_policy: Json,
    pub profile_policy_version: i32,
    /// Part VI §51: owning Platform Access tenant (environment-owned spaces only).
    pub platform_tenant_id: Option<Uuid>,
    /// Owning Platform Access application.
    pub platform_application_id: Option<Uuid>,
    /// Owning Platform Access application environment.
    pub platform_environment_id: Option<Uuid>,
    /// The service account whose credential created the space.
    pub created_by_service_account_id: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::conference_sessions::Entity")]
    ConferenceSessions,
    #[sea_orm(has_many = "super::meeting_members::Entity")]
    Members,
    #[sea_orm(has_many = "super::meeting_invitations::Entity")]
    Invitations,
    #[sea_orm(has_many = "super::meeting_recurrence_rules::Entity")]
    RecurrenceRules,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::OwnerUserId",
        to = "super::users::Column::Id"
    )]
    Owner,
}

impl Related<super::conference_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ConferenceSessions.def()
    }
}

impl Related<super::meeting_members::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Members.def()
    }
}

impl Related<super::meeting_invitations::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Invitations.def()
    }
}

impl Related<super::meeting_recurrence_rules::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RecurrenceRules.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
