//! MeetingInvitation — hash-only bearer invitation.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{InvitationStatus, Role};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_invitations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    /// `None` when issued by an environment's machine caller (no CF Meet account).
    pub inviter_user_id: Option<Uuid>,
    pub invitee_email: Option<String>,
    pub requested_role: Role,
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub expires_at: DateTimeUtc,
    pub use_limit: Option<i32>,
    pub usage_count: i32,
    pub status: InvitationStatus,
    pub revoked_at: Option<DateTimeUtc>,
    pub revoked_by: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::meeting_spaces::Entity",
        from = "Column::MeetingSpaceId",
        to = "super::meeting_spaces::Column::Id"
    )]
    MeetingSpace,
}

impl Related<super::meeting_spaces::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MeetingSpace.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
