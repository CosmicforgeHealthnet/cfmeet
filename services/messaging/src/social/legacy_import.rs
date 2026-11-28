//! ADR 0069 — one-time, idempotent import of the retired Meeting Service channels
//! (`export-social-channels` in the meeting-service image) into Messaging's own tables.
//!
//! Same pattern as the organizations cutover: the source service exports a read-only snapshot
//! of its own tables; this service imports the document into its own database and object
//! storage. Nothing here reads another service's database.
//!
//! * The document's checksum is recomputed (same algorithm as
//!   `backend::services::social_export::checksum`); a mismatch, another schema version or
//!   another environment than the operator named is refused and nothing is written.
//! * Ids are kept (channels, posts, media), so the import is idempotent: every insert is
//!   `ON CONFLICT DO NOTHING`, media already present is not stored again, and a repeated import
//!   reports everything `unchanged`.
//! * Media is checked like an upload (kind, type, size, recognised content, SHA-256) and
//!   stored in 4 MiB parts in the protected bucket before its rows are written; refused media
//!   is reported, and its post keeps its text (a media-only post is skipped).
//! * No content is ever printed (the report holds counts and ids only).
use std::collections::HashSet;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::media::{object_key, sniff_matches, MediaKind, PART_BYTES};
use crate::db;

pub const SCHEMA_VERSION: u32 = 1;
pub const SOURCE: &str = "cfmeet.meeting-service.social-channels";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: Uuid,
    pub owner_principal_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Follower {
    pub channel_id: Uuid,
    pub principal_id: Uuid,
    pub followed_at: DateTime<Utc>,
    pub last_read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author_principal_id: Uuid,
    pub kind: String,
    pub text: Option<String>,
    pub media_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub post_id: Uuid,
    pub principal_id: Uuid,
    pub emoji: String,
    pub reacted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub id: Uuid,
    pub owner_principal_id: Uuid,
    pub kind: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub sha256: String,
    pub data: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    pub schema_version: u32,
    pub source: String,
    pub scope: Scope,
    pub channels: Vec<Channel>,
    pub followers: Vec<Follower>,
    pub posts: Vec<Post>,
    pub reactions: Vec<Reaction>,
    pub media: Vec<Media>,
    pub checksum: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub channels: u64,
    pub followers: u64,
    pub posts: u64,
    pub reactions: u64,
    pub media: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    pub kind: &'static str,
    pub id: Uuid,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub imported: Counts,
    pub unchanged: Counts,
    pub refused: Vec<Refusal>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    Refused(String),
    #[error("database: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("object storage is not configured or failed")]
    Storage,
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The two-sided checksum (must equal the exporter's, line for line).
pub fn checksum(doc: &Export) -> String {
    let dash = || "-".to_string();
    let mut text = String::new();
    for c in &doc.channels {
        text.push_str(&format!(
            "channel|{}|{}|{}|{}|{}\n",
            c.id,
            c.owner_principal_id,
            hex_sha256(c.name.as_bytes()),
            c.description
                .as_deref()
                .map_or_else(dash, |d| hex_sha256(d.as_bytes())),
            c.created_at.timestamp_micros()
        ));
    }
    for f in &doc.followers {
        text.push_str(&format!(
            "follower|{}|{}|{}\n",
            f.channel_id,
            f.principal_id,
            f.followed_at.timestamp_micros()
        ));
    }
    for p in &doc.posts {
        text.push_str(&format!(
            "post|{}|{}|{}|{}|{}|{}|{}\n",
            p.id,
            p.channel_id,
            p.author_principal_id,
            p.kind,
            p.text
                .as_deref()
                .map_or_else(dash, |t| hex_sha256(t.as_bytes())),
            p.media_id.map_or_else(dash, |m| m.to_string()),
            p.created_at.timestamp_micros()
        ));
    }
    for r in &doc.reactions {
        text.push_str(&format!(
            "reaction|{}|{}|{}\n",
            r.post_id,
            r.principal_id,
            hex_sha256(r.emoji.as_bytes())
        ));
    }
    for m in &doc.media {
        text.push_str(&format!(
            "media|{}|{}|{}|{}|{}\n",
            m.id, m.owner_principal_id, m.kind, m.size_bytes, m.sha256
        ));
    }
    hex_sha256(text.as_bytes())
}

/// Validate and decode one media item (the same rules as an upload).
fn decode(m: &Media) -> Result<(MediaKind, String, Vec<u8>), &'static str> {
    let kind = MediaKind::parse(&m.kind).ok_or("UNKNOWN_KIND")?;
    let content_type = m.content_type.trim().to_ascii_lowercase();
    if content_type.len() > 64 || !kind.accepts(&content_type) {
        return Err("UNSUPPORTED_TYPE");
    }
    if m.size_bytes <= 0 || m.size_bytes > kind.max_bytes() {
        return Err("TOO_LARGE");
    }
    if m.duration_ms.is_some_and(|d| !(0..=3_600_000).contains(&d)) {
        return Err("BAD_DURATION");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(m.data.as_bytes())
        .map_err(|_| "BAD_ENCODING")?;
    if bytes.len() as i64 != m.size_bytes || hex_sha256(&bytes) != m.sha256 {
        return Err("INTEGRITY");
    }
    if !sniff_matches(kind, &bytes[..bytes.len().min(64)]) {
        return Err("UNRECOGNISED_CONTENT");
    }
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Ok((kind, content_type, bytes))
}

async fn exists<C: ConnectionTrait>(db: &C, sql: &str, id: Uuid) -> Result<bool, sea_orm::DbErr> {
    Ok(db::scalar_i64(db, sql, vec![id.into()]).await? > 0)
}

/// Import a parsed export. `environment` must name the document's environment (the
/// operator's confirmation that this is the right Messaging deployment).
pub async fn import(
    db: &DatabaseConnection,
    storage: &dyn cf_object_storage::ObjectStore,
    environment: Uuid,
    doc: &Export,
    now: DateTime<Utc>,
) -> Result<Report, ImportError> {
    if doc.schema_version != SCHEMA_VERSION || doc.source != SOURCE {
        return Err(ImportError::Refused(format!(
            "not a {SOURCE} export of schema version {SCHEMA_VERSION}"
        )));
    }
    if doc.scope.environment_id != environment {
        return Err(ImportError::Refused(
            "the export is for another environment than --environment".into(),
        ));
    }
    if checksum(doc) != doc.checksum {
        return Err(ImportError::Refused(
            "checksum mismatch: the export is incomplete or was changed".into(),
        ));
    }
    let scope = &doc.scope;
    let mut report = Report::default();

    // 1. Media: stored in the bucket first, then recorded (a failed run leaves only objects
    //    a re-run overwrites with the same bytes).
    let mut usable: HashSet<Uuid> = HashSet::new();
    for m in &doc.media {
        if exists(
            db,
            "SELECT count(*) FROM messaging_social_media WHERE id = $1",
            m.id,
        )
        .await?
        {
            usable.insert(m.id);
            report.unchanged.media += 1;
            continue;
        }
        let (kind, content_type, bytes) = match decode(m) {
            Ok(v) => v,
            Err(reason) => {
                report.refused.push(Refusal {
                    kind: "media",
                    id: m.id,
                    reason,
                });
                continue;
            }
        };
        if !storage.configured() {
            return Err(ImportError::Storage);
        }
        let mut parts = Vec::new();
        for (i, chunk) in bytes.chunks(PART_BYTES as usize).enumerate() {
            let part = i as i32;
            storage
                .put(
                    &object_key(scope.environment_id, m.id, part),
                    chunk.to_vec(),
                    &content_type,
                )
                .await
                .map_err(|_| ImportError::Storage)?;
            parts.push((part, chunk.len() as i32, hex_sha256(chunk)));
        }
        let txn = db.begin().await?;
        let inserted = db::exec(
            &txn,
            "INSERT INTO messaging_social_media (id, tenant_id, application_id, environment_id, owner_principal_id, purpose, \
               kind, media_type, size_bytes, part_bytes, part_count, duration_ms, state, created_at, completed_at) \
             VALUES ($1, $2, $3, $4, $5, 'CHANNEL', $6, $7, $8, $9, $10, $11, 'COMPLETE', $12, $13) ON CONFLICT (id) DO NOTHING",
            vec![
                m.id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                m.owner_principal_id.into(),
                kind.as_str().into(),
                content_type.into(),
                m.size_bytes.into(),
                (PART_BYTES as i32).into(),
                (parts.len() as i32).into(),
                m.duration_ms.into(),
                m.created_at.into(),
                now.into(),
            ],
        )
        .await?;
        for (part, size, sha) in &parts {
            db::exec(
                &txn,
                "INSERT INTO messaging_social_media_parts (media_id, part, size_bytes, sha256, declared_at, stored_at) \
                 VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT DO NOTHING",
                vec![m.id.into(), (*part).into(), (*size).into(), sha.clone().into(), now.into()],
            )
            .await?;
        }
        txn.commit().await?;
        usable.insert(m.id);
        if inserted > 0 {
            report.imported.media += 1;
        } else {
            report.unchanged.media += 1;
        }
    }

    // 2. Channels, followers, posts and reactions in one transaction.
    let txn = db.begin().await?;
    let mut channels: HashSet<Uuid> = HashSet::new();
    for c in &doc.channels {
        let name = c.name.trim();
        if name.is_empty()
            || name.chars().count() > 80
            || c.description
                .as_ref()
                .is_some_and(|d| d.chars().count() > 500)
        {
            report.refused.push(Refusal {
                kind: "channel",
                id: c.id,
                reason: "INVALID",
            });
            continue;
        }
        // A channel id already used in another environment is never taken over.
        if let Some(env) = db::one::<EnvRow, _>(
            &txn,
            "SELECT environment_id FROM messaging_channels WHERE id = $1",
            vec![c.id.into()],
        )
        .await?
        {
            if env.environment_id != scope.environment_id {
                report.refused.push(Refusal {
                    kind: "channel",
                    id: c.id,
                    reason: "ID_IN_USE",
                });
                continue;
            }
            channels.insert(c.id);
            report.unchanged.channels += 1;
            continue;
        }
        db::exec(
            &txn,
            "INSERT INTO messaging_channels (id, tenant_id, application_id, environment_id, owner_principal_id, name, description, \
               created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (id) DO NOTHING",
            vec![
                c.id.into(),
                scope.tenant_id.into(),
                scope.application_id.into(),
                scope.environment_id.into(),
                c.owner_principal_id.into(),
                name.to_string().into(),
                c.description.clone().into(),
                c.created_at.into(),
                c.updated_at.into(),
            ],
        )
        .await?;
        channels.insert(c.id);
        report.imported.channels += 1;
    }
    for f in &doc.followers {
        if !channels.contains(&f.channel_id) {
            continue;
        }
        let n = db::exec(
            &txn,
            "INSERT INTO messaging_channel_followers (channel_id, principal_id, followed_at, last_read_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            vec![f.channel_id.into(), f.principal_id.into(), f.followed_at.into(), f.last_read_at.into()],
        )
        .await?;
        if n > 0 {
            report.imported.followers += 1
        } else {
            report.unchanged.followers += 1
        }
    }
    let mut posts: HashSet<Uuid> = HashSet::new();
    for p in &doc.posts {
        if !channels.contains(&p.channel_id) {
            continue;
        }
        let media = p.media_id.filter(|m| usable.contains(m));
        let kind = if media.is_some() {
            p.kind.as_str()
        } else {
            "TEXT"
        };
        if (media.is_none() && p.text.is_none())
            || !matches!(kind, "TEXT" | "IMAGE" | "VIDEO" | "AUDIO")
            || p.text.as_ref().is_some_and(|t| t.chars().count() > 4000)
        {
            report.refused.push(Refusal {
                kind: "post",
                id: p.id,
                reason: if media.is_none() && p.text.is_none() {
                    "MEDIA_REFUSED"
                } else {
                    "INVALID"
                },
            });
            continue;
        }
        let n = db::exec(
            &txn,
            "INSERT INTO messaging_channel_posts (id, channel_id, author_principal_id, kind, words, media_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
            vec![
                p.id.into(),
                p.channel_id.into(),
                p.author_principal_id.into(),
                kind.to_string().into(),
                p.text.clone().into(),
                media.into(),
                p.created_at.into(),
            ],
        )
        .await?;
        posts.insert(p.id);
        if n > 0 {
            report.imported.posts += 1
        } else {
            report.unchanged.posts += 1
        }
    }
    for r in &doc.reactions {
        if !posts.contains(&r.post_id) {
            continue;
        }
        let n = db::exec(
            &txn,
            "INSERT INTO messaging_channel_reactions (post_id, principal_id, emoji, reacted_at) VALUES ($1, $2, $3, $4) \
             ON CONFLICT DO NOTHING",
            vec![r.post_id.into(), r.principal_id.into(), r.emoji.clone().into(), r.reacted_at.into()],
        )
        .await?;
        if n > 0 {
            report.imported.reactions += 1
        } else {
            report.unchanged.reactions += 1
        }
    }
    txn.commit().await?;
    Ok(report)
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct EnvRow {
    environment_id: Uuid,
}
