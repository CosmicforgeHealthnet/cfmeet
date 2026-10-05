//! TranscriptionJob — one LIVE or POST_MEETING transcription run (Part G).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "transcription_jobs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub artifact_id: Uuid,
    pub mode: String,
    pub status: String,
    pub provider: String,
    #[serde(skip_serializing)]
    pub provider_job_ref: Option<String>,
    pub language: String,
    pub source_artifact_id: Option<Uuid>,
    pub requested_by: Option<String>,
    pub segment_count: i32,
    pub failure_code: Option<String>,
    pub started_at: Option<DateTimeUtc>,
    pub stop_requested_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
