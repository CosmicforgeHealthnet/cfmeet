//! MeetingConsent — the current per-participant consent decision per type and policy
//! version (Part G). History is kept in the audit log; types are independent.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_consents")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub participant_id: Uuid,
    pub consent_type: String,
    pub consent_version: String,
    /// GRANTED (API: ACCEPTED) / DECLINED / WITHDRAWN.
    pub decision: String,
    pub decided_at: DateTimeUtc,
    pub created_at: DateTimeUtc,
    pub withdrawn_at: Option<DateTimeUtc>,
    pub source: Option<String>,
    pub request_id: Option<String>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
