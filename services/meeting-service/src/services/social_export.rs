//! ADR 0069 — one-time export of the retired Meeting Service channels (channels, followers,
//! posts, reactions and the posts' media) for the Messaging import
//! (`messaging-admin social import-channels`). Statuses expire within 24 hours and are not
//! exported.
//!
//! Same pattern as the organizations cutover (`organization_export.rs`): one
//! `REPEATABLE READ, READ ONLY` snapshot of this service's OWN tables, printed as a document;
//! the receiving service imports it into its own database. Nothing reads another service's
//! database and the source is never written.
//!
//! People are exported as their Messaging principals (`communication_principal_links`, the
//! `USER` links of CF Meet's communication environment). A person without a link has never
//! used Messaging: their rows are counted in `skipped` (a channel whose owner is unlinked is
//! skipped with its posts and followers). Run the export again after they sign in; the import
//! is idempotent, so a second run only adds what is new.
use base64::Engine as _;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const EXPORT_SCHEMA_VERSION: u32 = 1;
pub const EXPORT_SOURCE: &str = "cfmeet.meeting-service.social-channels";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedChannel {
    pub id: Uuid,
    pub owner_principal_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedFollower {
    pub channel_id: Uuid,
    pub principal_id: Uuid,
    pub followed_at: DateTime<Utc>,
    pub last_read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedPost {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub author_principal_id: Uuid,
    pub kind: String,
    pub text: Option<String>,
    pub media_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedReaction {
    pub post_id: Uuid,
    pub principal_id: Uuid,
    pub emoji: String,
    pub reacted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportedMedia {
    pub id: Uuid,
    pub owner_principal_id: Uuid,
    pub kind: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub sha256: String,
    /// The whole file, standard base64.
    pub data: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportCounts {
    pub channels: usize,
    pub followers: usize,
    pub posts: usize,
    pub reactions: usize,
    pub media: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    /// People with rows here but no Messaging principal yet.
    pub unlinked_people: usize,
    pub channels: usize,
    pub followers: usize,
    pub posts: usize,
    pub reactions: usize,
    /// Unfinished or partially stored media (its post is exported without it).
    pub media: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialChannelsExport {
    pub schema_version: u32,
    pub source: &'static str,
    pub exported_at: DateTime<Utc>,
    pub scope: ExportScope,
    pub channels: Vec<ExportedChannel>,
    pub followers: Vec<ExportedFollower>,
    pub posts: Vec<ExportedPost>,
    pub reactions: Vec<ExportedReaction>,
    pub media: Vec<ExportedMedia>,
    pub counts: ExportCounts,
    pub skipped: Skipped,
    /// SHA-256 over one line per record (see [`checksum`]); the import recomputes it.
    pub checksum: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("database: {0}")]
    Database(#[from] sea_orm::DbErr),
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn opt<T: ToString>(v: &Option<T>) -> String {
    v.as_ref()
        .map_or_else(|| "-".to_string(), |x| x.to_string())
}

/// The two-sided checksum: one line per record, in this order and sorted as exported (by
/// id / key), each terminated by `\n`; text fields enter as their SHA-256. Messaging's import
/// (`messaging::social::legacy_import::checksum`) computes the same value.
pub fn checksum(doc: &SocialChannelsExport) -> String {
    let mut text = String::new();
    for c in &doc.channels {
        text.push_str(&format!(
            "channel|{}|{}|{}|{}|{}\n",
            c.id,
            c.owner_principal_id,
            hex_sha256(c.name.as_bytes()),
            c.description
                .as_deref()
                .map_or_else(|| "-".to_string(), |d| hex_sha256(d.as_bytes())),
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
                .map_or_else(|| "-".to_string(), |t| hex_sha256(t.as_bytes())),
            opt(&p.media_id),
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

async fn rows<C: ConnectionTrait>(
    db: &C,
    sql: &str,
) -> Result<Vec<sea_orm::QueryResult>, sea_orm::DbErr> {
    db.query_all(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
}

/// Read every retired channel in one consistent, read-only snapshot.
pub async fn export(
    db: &DatabaseConnection,
    scope: ExportScope,
) -> Result<SocialChannelsExport, ExportError> {
    let txn = db.begin().await?;
    txn.execute_unprepared("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await?;
    let links: HashMap<Uuid, Uuid> = txn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT subject_id, principal_id FROM communication_principal_links \
             WHERE subject_kind = 'USER' AND environment_id = $1",
            vec![scope.environment_id.into()],
        ))
        .await?
        .iter()
        .map(|r| Ok((r.try_get("", "subject_id")?, r.try_get("", "principal_id")?)))
        .collect::<Result<_, sea_orm::DbErr>>()?;
    let channel_rows = rows(&txn, "SELECT id, owner_user_id, name, description, created_at, updated_at FROM social_channels ORDER BY id").await?;
    let follower_rows = rows(&txn, "SELECT channel_id, user_id, followed_at, last_read_at FROM social_channel_followers ORDER BY channel_id, user_id").await?;
    let post_rows = rows(&txn, "SELECT id, channel_id, author_user_id, kind, body_text, media_id, created_at FROM social_channel_posts ORDER BY id").await?;
    let reaction_rows = rows(&txn, "SELECT post_id, user_id, emoji, reacted_at FROM social_channel_reactions ORDER BY post_id, user_id").await?;
    let exported_at: DateTime<Utc> = txn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT now() AS at".to_owned(),
        ))
        .await?
        .ok_or_else(|| sea_orm::DbErr::Custom("clock unavailable".into()))?
        .try_get("", "at")?;

    let mut skipped = Skipped::default();
    let mut unlinked: HashSet<Uuid> = HashSet::new();
    let mut principal = |user: Uuid| match links.get(&user) {
        Some(p) => Some(*p),
        None => {
            unlinked.insert(user);
            None
        }
    };

    let mut channels = Vec::new();
    for r in &channel_rows {
        let owner: Uuid = r.try_get("", "owner_user_id")?;
        let Some(owner) = principal(owner) else {
            skipped.channels += 1;
            continue;
        };
        channels.push(ExportedChannel {
            id: r.try_get("", "id")?,
            owner_principal_id: owner,
            name: r.try_get("", "name")?,
            description: r.try_get("", "description")?,
            created_at: r.try_get("", "created_at")?,
            updated_at: r.try_get("", "updated_at")?,
        });
    }
    let kept: HashSet<Uuid> = channels.iter().map(|c| c.id).collect();

    let mut followers = Vec::new();
    for r in &follower_rows {
        let channel: Uuid = r.try_get("", "channel_id")?;
        let user: Uuid = r.try_get("", "user_id")?;
        match (kept.contains(&channel), principal(user)) {
            (true, Some(p)) => followers.push(ExportedFollower {
                channel_id: channel,
                principal_id: p,
                followed_at: r.try_get("", "followed_at")?,
                last_read_at: r.try_get("", "last_read_at")?,
            }),
            _ => skipped.followers += 1,
        }
    }

    let mut posts = Vec::new();
    for r in &post_rows {
        let channel: Uuid = r.try_get("", "channel_id")?;
        let author: Uuid = r.try_get("", "author_user_id")?;
        match (kept.contains(&channel), principal(author)) {
            (true, Some(p)) => posts.push(ExportedPost {
                id: r.try_get("", "id")?,
                channel_id: channel,
                author_principal_id: p,
                kind: r.try_get("", "kind")?,
                text: r.try_get("", "body_text")?,
                media_id: r.try_get("", "media_id")?,
                created_at: r.try_get("", "created_at")?,
            }),
            _ => skipped.posts += 1,
        }
    }
    let kept_posts: HashSet<Uuid> = posts.iter().map(|p| p.id).collect();

    let mut reactions = Vec::new();
    for r in &reaction_rows {
        let post: Uuid = r.try_get("", "post_id")?;
        let user: Uuid = r.try_get("", "user_id")?;
        match (kept_posts.contains(&post), principal(user)) {
            (true, Some(p)) => reactions.push(ExportedReaction {
                post_id: post,
                principal_id: p,
                emoji: r.try_get("", "emoji")?,
                reacted_at: r.try_get("", "reacted_at")?,
            }),
            _ => skipped.reactions += 1,
        }
    }

    // The posts' media, whole and complete only.
    let mut media = Vec::new();
    let mut usable: HashSet<Uuid> = HashSet::new();
    let mut wanted: Vec<Uuid> = posts.iter().filter_map(|p| p.media_id).collect();
    wanted.sort();
    wanted.dedup();
    for id in wanted {
        let Some(m) = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT owner_user_id, kind, content_type, size_bytes, chunk_count, duration_ms, created_at, completed_at \
                 FROM social_media WHERE id = $1",
                vec![id.into()],
            ))
            .await?
        else {
            skipped.media += 1;
            continue;
        };
        let completed: Option<DateTime<Utc>> = m.try_get("", "completed_at")?;
        let owner: Uuid = m.try_get("", "owner_user_id")?;
        let chunk_count: i32 = m.try_get("", "chunk_count")?;
        let size: i64 = m.try_get("", "size_bytes")?;
        let chunks = txn
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT seq, bytes FROM social_media_chunks WHERE media_id = $1 ORDER BY seq",
                vec![id.into()],
            ))
            .await?;
        let mut data = Vec::with_capacity(size.max(0) as usize);
        let mut in_order = chunks.len() == chunk_count as usize;
        for (i, c) in chunks.iter().enumerate() {
            let seq: i32 = c.try_get("", "seq")?;
            in_order &= seq == i as i32;
            let bytes: Vec<u8> = c.try_get("", "bytes")?;
            data.extend_from_slice(&bytes);
        }
        let Some(owner) = principal(owner) else {
            skipped.media += 1;
            continue;
        };
        if completed.is_none() || !in_order || data.len() as i64 != size {
            skipped.media += 1;
            continue;
        }
        usable.insert(id);
        media.push(ExportedMedia {
            id,
            owner_principal_id: owner,
            kind: m.try_get("", "kind")?,
            content_type: m.try_get("", "content_type")?,
            size_bytes: size,
            duration_ms: m.try_get("", "duration_ms")?,
            created_at: m.try_get("", "created_at")?,
            sha256: hex_sha256(&data),
            data: base64::engine::general_purpose::STANDARD.encode(&data),
        });
    }
    txn.commit().await?;
    // A post whose media could not be exported keeps its text; a media-only post is skipped.
    posts.retain_mut(|p| match p.media_id {
        Some(m) if !usable.contains(&m) => {
            p.media_id = None;
            if p.text.is_none() {
                skipped.posts += 1;
                return false;
            }
            p.kind = "TEXT".into();
            true
        }
        _ => true,
    });
    let kept_posts: HashSet<Uuid> = posts.iter().map(|p| p.id).collect();
    let before = reactions.len();
    reactions.retain(|r| kept_posts.contains(&r.post_id));
    skipped.reactions += before - reactions.len();
    skipped.unlinked_people = unlinked.len();

    let mut doc = SocialChannelsExport {
        schema_version: EXPORT_SCHEMA_VERSION,
        source: EXPORT_SOURCE,
        exported_at,
        scope,
        counts: ExportCounts {
            channels: channels.len(),
            followers: followers.len(),
            posts: posts.len(),
            reactions: reactions.len(),
            media: media.len(),
        },
        channels,
        followers,
        posts,
        reactions,
        media,
        skipped,
        checksum: String::new(),
    };
    doc.checksum = checksum(&doc);
    Ok(doc)
}
