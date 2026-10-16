use sea_orm::entity::prelude::*;

use crate::calls::kinds::{AttemptOutcome, FailureCategory};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_attempts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub call_id: Uuid,
    pub route_id: Option<Uuid>,
    pub leg_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub attempt_number: i32,
    pub previous_attempt_id: Option<Uuid>,
    pub outcome: AttemptOutcome,
    pub unknown_outcome: bool,
    pub failure_category: Option<FailureCategory>,
    pub started_at: DateTimeUtc,
    pub finished_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
