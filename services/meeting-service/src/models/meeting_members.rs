//! MeetingMember — persistent membership of a MeetingSpace (not a conference participant).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{MemberStatus, PrincipalType, Role};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_members")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub principal_type: PrincipalType,
    pub principal_ref: String,
    pub user_id: Option<Uuid>,
    pub role: Role,
    pub status: MemberStatus,
    pub created_by: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub revoked_at: Option<DateTimeUtc>,
    pub revoked_by: Option<Uuid>,
    pub version: i32,
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
