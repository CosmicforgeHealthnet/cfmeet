//! WebinarRegistration — approved registration fields only (Part H). The admission
//! token is a normal hashed `meeting_invitations` row; registration data never
//! becomes RTC identity authority until admission resolves it.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "webinar_registrations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub status: String,
    pub display_name: String,
    pub email: Option<String>,
    pub organization_name: Option<String>,
    pub custom_fields: Json,
    pub user_id: Option<Uuid>,
    pub invitation_id: Uuid,
    #[serde(skip_serializing)]
    pub idempotency_key_digest: Option<String>,
    #[serde(skip_serializing)]
    pub request_digest: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub cancelled_at: Option<DateTimeUtc>,
    pub cancelled_by: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
