//! Calling-private record of authenticated media-provider callbacks (no raw payload).

use sea_orm::entity::prelude::*;

use crate::calls::kinds::{MediaEventKind, MediaEventOutcome, MediaIdentityStatus};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "call_media_provider_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub provider_adapter: String,
    pub provider_event_id: String,
    pub event_kind: MediaEventKind,
    pub call_id: Option<Uuid>,
    pub media_session_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub identity_status: MediaIdentityStatus,
    pub outcome: Option<MediaEventOutcome>,
    pub occurred_at: Option<DateTimeUtc>,
    pub received_at: DateTimeUtc,
    pub processed_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
