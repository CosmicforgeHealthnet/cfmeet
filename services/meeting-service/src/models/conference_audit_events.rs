//! ConferenceAuditEvent — append-only meeting-domain audit (DB trigger blocks UPDATE/DELETE).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "conference_audit_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub actor_type: String,
    pub actor_ref: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_ref: Option<String>,
    pub metadata: Json,
    pub request_id: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
