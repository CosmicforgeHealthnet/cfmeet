//! TranscriptSegment — a FINAL transcript segment (Part G). PARTIAL segments are
//! ephemeral and never stored. Speaker is an internal participant reference only.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "transcript_segments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub conference_id: Uuid,
    pub job_id: Uuid,
    pub artifact_id: Uuid,
    #[serde(skip_serializing)]
    pub provider_segment_id: String,
    pub sequence: i64,
    pub speaker_participant_id: Option<Uuid>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub language: String,
    pub text: String,
    pub confidence: Option<f32>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
