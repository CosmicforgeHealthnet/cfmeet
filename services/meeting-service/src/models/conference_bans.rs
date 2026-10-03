//! ConferenceBan — persistent ban authority (space- or conference-scoped).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::{BanScope, BanSubjectType};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "conference_bans")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub scope: BanScope,
    pub subject_type: BanSubjectType,
    pub subject_ref: String,
    pub participant_id: Option<Uuid>,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTimeUtc,
    pub expires_at: Option<DateTimeUtc>,
    pub revoked_at: Option<DateTimeUtc>,
    pub revoked_by: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
