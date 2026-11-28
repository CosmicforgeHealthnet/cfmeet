use sea_orm_migration::prelude::*;

/// Updates (WhatsApp-style): 24-hour status posts seen by the poster's connected contacts,
/// and channels people follow. Media (photos, videos, voice notes) is stored in 512 KiB
/// chunks; status media is deleted with its posts after 24 hours.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE social_media (
 id uuid PRIMARY KEY,
 owner_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 purpose varchar(8) NOT NULL CHECK (purpose IN ('STATUS','CHANNEL')),
 kind varchar(8) NOT NULL CHECK (kind IN ('IMAGE','VIDEO','AUDIO')),
 content_type varchar(64) NOT NULL,
 size_bytes bigint NOT NULL CHECK (size_bytes > 0 AND size_bytes <= 67108864),
 chunk_count integer NOT NULL CHECK (chunk_count BETWEEN 1 AND 128),
 duration_ms integer CHECK (duration_ms IS NULL OR duration_ms BETWEEN 0 AND 3600000),
 completed_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX social_media_owner ON social_media(owner_user_id, created_at);
CREATE TABLE social_media_chunks (
 media_id uuid NOT NULL REFERENCES social_media(id) ON DELETE CASCADE,
 seq integer NOT NULL CHECK (seq >= 0),
 bytes bytea NOT NULL,
 PRIMARY KEY (media_id, seq)
);
ALTER TABLE social_media_chunks ALTER COLUMN bytes SET STORAGE EXTERNAL;
CREATE TABLE status_posts (
 id uuid PRIMARY KEY,
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 kind varchar(8) NOT NULL CHECK (kind IN ('TEXT','IMAGE','VIDEO','AUDIO')),
 body_text varchar(700),
 background varchar(16),
 font smallint NOT NULL DEFAULT 0 CHECK (font BETWEEN 0 AND 4),
 media_id uuid REFERENCES social_media(id) ON DELETE CASCADE,
 start_ms integer,
 end_ms integer,
 caption varchar(700),
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL,
 CHECK ((kind = 'TEXT') = (media_id IS NULL)),
 CHECK (kind <> 'TEXT' OR body_text IS NOT NULL),
 CHECK (start_ms IS NULL OR (start_ms >= 0 AND end_ms > start_ms AND end_ms - start_ms <= 60000))
);
CREATE INDEX status_posts_live ON status_posts(user_id, expires_at);
CREATE INDEX status_posts_expiry ON status_posts(expires_at);
CREATE INDEX status_posts_media ON status_posts(media_id);
CREATE TABLE status_views (
 post_id uuid NOT NULL REFERENCES status_posts(id) ON DELETE CASCADE,
 viewer_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 viewed_at timestamptz NOT NULL DEFAULT now(),
 liked boolean NOT NULL DEFAULT false,
 PRIMARY KEY (post_id, viewer_user_id)
);
CREATE TABLE social_channels (
 id uuid PRIMARY KEY,
 owner_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 name varchar(80) NOT NULL,
 description varchar(500),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX social_channels_name ON social_channels(lower(name));
CREATE INDEX social_channels_owner ON social_channels(owner_user_id);
CREATE TABLE social_channel_followers (
 channel_id uuid NOT NULL REFERENCES social_channels(id) ON DELETE CASCADE,
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 followed_at timestamptz NOT NULL DEFAULT now(),
 last_read_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (channel_id, user_id)
);
CREATE INDEX social_channel_followers_user ON social_channel_followers(user_id);
CREATE TABLE social_channel_posts (
 id uuid PRIMARY KEY,
 channel_id uuid NOT NULL REFERENCES social_channels(id) ON DELETE CASCADE,
 author_user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 body_text varchar(4000),
 media_id uuid REFERENCES social_media(id) ON DELETE SET NULL,
 kind varchar(8) NOT NULL CHECK (kind IN ('TEXT','IMAGE','VIDEO','AUDIO')),
 created_at timestamptz NOT NULL DEFAULT now(),
 CHECK (body_text IS NOT NULL OR media_id IS NOT NULL)
);
CREATE INDEX social_channel_posts_feed ON social_channel_posts(channel_id, created_at DESC);
CREATE INDEX social_channel_posts_media ON social_channel_posts(media_id);
CREATE TABLE social_channel_reactions (
 post_id uuid NOT NULL REFERENCES social_channel_posts(id) ON DELETE CASCADE,
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 emoji varchar(16) NOT NULL,
 reacted_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (post_id, user_id)
);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE social_channel_reactions; DROP TABLE social_channel_posts; \
                 DROP TABLE social_channel_followers; DROP TABLE social_channels; \
                 DROP TABLE status_views; DROP TABLE status_posts; \
                 DROP TABLE social_media_chunks; DROP TABLE social_media;",
            )
            .await?;
        Ok(())
    }
}
