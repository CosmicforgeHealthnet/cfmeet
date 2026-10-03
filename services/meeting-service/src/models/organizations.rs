//! Organization — CF account organization (authoritative for ORGANIZATION_ONLY).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::OrganizationStatus;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "organizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub status: OrganizationStatus,
    pub created_by: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::organization_memberships::Entity")]
    Memberships,
}

impl Related<super::organization_memberships::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Memberships.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
