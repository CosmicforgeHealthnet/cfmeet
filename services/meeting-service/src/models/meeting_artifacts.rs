//! MeetingArtifact — unified post-meeting artefact (Part G): recordings, transcripts,
//! caption tracks, reports, summaries.
//!
//! `status` is the authoritative lifecycle (REQUESTED → RECORDING → STOPPING →
//! PROCESSING → READY → DELETED, FAILED). Bytes live in object storage; the row keeps
//! only the opaque `storage_bucket` / `storage_key` reference — never a public URL.
//! Provider references are internal and never serialized.

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
    #[serde(skip_serializing)]
    pub provider: Option<String>,
    #[serde(skip_serializing)]
    pub provider_ref: Option<String>,
    /// Legacy Part B storage state (kept in sync with `status`).
    pub storage_state: String,
    #[serde(skip_serializing)]
    pub storage_uri: Option<String>,
    pub size_bytes: Option<i64>,
    pub checksum_sha256: Option<String>,
    pub retention_policy_ref: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub available_at: Option<DateTimeUtc>,
    pub deleted_at: Option<DateTimeUtc>,
    pub status: String,
    #[serde(skip_serializing)]
    pub storage_bucket: Option<String>,
    #[serde(skip_serializing)]
    pub storage_key: Option<String>,
    pub content_type: Option<String>,
    pub duration_ms: Option<i64>,
    pub layout: Option<String>,
    pub metadata: Json,
    pub retention_at: Option<DateTimeUtc>,
    pub started_at: Option<DateTimeUtc>,
    pub ended_at: Option<DateTimeUtc>,
    pub finalized_at: Option<DateTimeUtc>,
    pub stop_requested_at: Option<DateTimeUtc>,
    pub delete_requested_at: Option<DateTimeUtc>,
    pub delete_reason: Option<String>,
    pub failure_code: Option<String>,
    #[serde(skip_serializing)]
    pub failure_detail: Option<String>,
    pub attempts: i32,
    pub next_attempt_at: Option<DateTimeUtc>,
    #[serde(skip_serializing)]
    pub locked_by: Option<String>,
    pub locked_until: Option<DateTimeUtc>,
    pub verified_at: Option<DateTimeUtc>,
    pub request_id: Option<String>,
    #[serde(skip_serializing)]
    pub idempotency_key: Option<String>,
    pub source_artifact_id: Option<Uuid>,
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
