//! MeetingRecurrenceException — single-occurrence exception keyed by the original
//! wall-clock start in the series timezone (independent of materialization).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::domain::RecurrenceExceptionKind;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_recurrence_exceptions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub recurrence_rule_id: Uuid,
    pub meeting_space_id: Uuid,
    pub original_start_local: DateTime,
    pub kind: RecurrenceExceptionKind,
    pub new_start_at: Option<DateTimeUtc>,
    pub new_end_at: Option<DateTimeUtc>,
    pub created_by: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
