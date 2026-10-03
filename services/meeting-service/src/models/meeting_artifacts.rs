//! MeetingArtifact — recordings, transcripts and other conference artefacts (Part G).
//!
//! Schema foundation only — behaviour is delivered by a later Phase 2 part.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "meeting_artifacts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Uuid,
    pub artifact_type: String,
    pub provider: Option<String>,
    pub provider_ref: Option<String>,
    pub storage_state: String,
    pub storage_uri: Option<String>,
    pub size_bytes: Option<i64>,
    pub checksum_sha256: Option<String>,
    pub retention_policy_ref: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub available_at: Option<DateTimeUtc>,
    pub deleted_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
