//! Channels, complete (ADR 0072; additive; the second migration of the separate
//! `messaging_social_seaql_migrations` history, which no released image before ADR 0069
//! reads, so a rollback to the previous image still boots on this schema).
//!
//! * `messaging_channels` gains a unique handle per environment, category, language,
//!   country, PUBLIC / PRIVATE visibility, a profile picture, the verification badge
//!   (`verified_at`, set only by HQ through the admin bridge), the moderation state
//!   (ACTIVE / RESTRICTED / SUSPENDED / BANNED, set only by HQ), a denormalized follower
//!   count (discovery's keyset order), `stats_since` (when analytics recording began) and a
//!   row `version` for the bridge's commands.
//! * `messaging_channel_followers.muted`.
//! * Posts gain VOICE / DOCUMENT / GIF / STICKER / POLL kinds, `edited_at`, `pinned_at` and
//!   a client-supplied link preview; media gains the DOCUMENT kind and a file name.
//! * New: admins with permissions, invite links (SHA-256 of the token only), scheduled
//!   posts (moved into posts by the sweep, so an older image never shows one early), polls
//!   and their votes (one row per voter and option), post views (one row per person, the
//!   dedupe), follow / unfollow events and forwards (no principal: analytics are counts),
//!   starred posts, reports, verification applications, HQ moderation actions and the
//!   channel's own admin audit log.
//!
//! No column holds a follower's contact details. Principals are opaque Platform Access ids.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_channels
    ADD COLUMN handle                 VARCHAR(32),
    ADD COLUMN category               VARCHAR(24),
    ADD COLUMN language               VARCHAR(8),
    ADD COLUMN country                VARCHAR(2),
    ADD COLUMN visibility             VARCHAR(8)   NOT NULL DEFAULT 'PUBLIC',
    ADD COLUMN avatar_media_id        UUID,
    ADD COLUMN verified_at            TIMESTAMPTZ,
    ADD COLUMN moderation_state       VARCHAR(10)  NOT NULL DEFAULT 'ACTIVE',
    ADD COLUMN moderation_reason      VARCHAR(48),
    ADD COLUMN moderation_changed_at  TIMESTAMPTZ,
    ADD COLUMN follower_count         BIGINT       NOT NULL DEFAULT 0,
    ADD COLUMN stats_since            TIMESTAMPTZ  NOT NULL DEFAULT now(),
    ADD COLUMN version                BIGINT       NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_messaging_channels_handle CHECK (handle IS NULL OR handle ~ '^[a-z][a-z0-9_]{3,30}[a-z0-9]$'),
    ADD CONSTRAINT ck_messaging_channels_category CHECK (category IS NULL OR category ~ '^[A-Z][A-Z_]{1,23}$'),
    ADD CONSTRAINT ck_messaging_channels_language CHECK (language IS NULL OR language ~ '^[a-z]{2,3}(-[A-Z]{2})?$'),
    ADD CONSTRAINT ck_messaging_channels_country CHECK (country IS NULL OR country ~ '^[A-Z]{2}$'),
    ADD CONSTRAINT ck_messaging_channels_visibility CHECK (visibility IN ('PUBLIC', 'PRIVATE')),
    ADD CONSTRAINT ck_messaging_channels_moderation CHECK (moderation_state IN ('ACTIVE', 'RESTRICTED', 'SUSPENDED', 'BANNED')),
    ADD CONSTRAINT ck_messaging_channels_version CHECK (version >= 1);
UPDATE messaging_channels c SET follower_count = (SELECT count(*) FROM messaging_channel_followers f WHERE f.channel_id = c.id);
CREATE UNIQUE INDEX ux_messaging_channels_handle ON messaging_channels (environment_id, handle) WHERE handle IS NOT NULL;
CREATE INDEX ix_messaging_channels_popular ON messaging_channels (environment_id, follower_count DESC, id DESC)
    WHERE visibility = 'PUBLIC' AND moderation_state = 'ACTIVE';
CREATE INDEX ix_messaging_channels_new ON messaging_channels (environment_id, created_at DESC, id DESC)
    WHERE visibility = 'PUBLIC' AND moderation_state = 'ACTIVE';
CREATE INDEX ix_messaging_channels_verified ON messaging_channels (environment_id, follower_count DESC, id DESC)
    WHERE verified_at IS NOT NULL AND visibility = 'PUBLIC' AND moderation_state = 'ACTIVE';
CREATE INDEX ix_messaging_channels_filters ON messaging_channels (environment_id, category, country, language);
CREATE INDEX ix_messaging_channels_search ON messaging_channels
    USING GIN (to_tsvector('simple', name || ' ' || coalesce(handle, '') || ' ' || coalesce(description, '')));

ALTER TABLE messaging_channel_followers ADD COLUMN muted BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE messaging_social_media DROP CONSTRAINT ck_messaging_social_media_kind;
ALTER TABLE messaging_social_media ALTER COLUMN kind TYPE VARCHAR(8);
ALTER TABLE messaging_social_media
    ADD CONSTRAINT ck_messaging_social_media_kind CHECK (kind IN ('IMAGE', 'VIDEO', 'AUDIO', 'DOCUMENT')),
    ADD COLUMN file_name VARCHAR(200);

ALTER TABLE messaging_channel_posts DROP CONSTRAINT ck_messaging_channel_posts_kind;
ALTER TABLE messaging_channel_posts ALTER COLUMN kind TYPE VARCHAR(8);
ALTER TABLE messaging_channel_posts
    ADD CONSTRAINT ck_messaging_channel_posts_kind
        CHECK (kind IN ('TEXT', 'IMAGE', 'VIDEO', 'AUDIO', 'VOICE', 'DOCUMENT', 'GIF', 'STICKER', 'POLL')),
    ADD COLUMN edited_at     TIMESTAMPTZ,
    ADD COLUMN pinned_at     TIMESTAMPTZ,
    ADD COLUMN link_preview  JSONB;
CREATE INDEX ix_messaging_channel_posts_pinned ON messaging_channel_posts (channel_id, pinned_at DESC) WHERE pinned_at IS NOT NULL;
CREATE INDEX ix_messaging_channel_reactions_time ON messaging_channel_reactions (post_id, reacted_at);

CREATE TABLE messaging_channel_admins (
    channel_id             UUID         NOT NULL,
    principal_id           UUID         NOT NULL,
    can_post               BOOLEAN      NOT NULL,
    can_edit               BOOLEAN      NOT NULL,
    can_manage_admins      BOOLEAN      NOT NULL,
    added_by_principal_id  UUID         NOT NULL,
    added_at               TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_admins PRIMARY KEY (channel_id, principal_id),
    CONSTRAINT fk_messaging_channel_admins_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_channel_admins_principal ON messaging_channel_admins (principal_id);

CREATE TABLE messaging_channel_invites (
    id                       UUID         NOT NULL,
    channel_id               UUID         NOT NULL,
    token_sha256             CHAR(64)     NOT NULL,
    created_by_principal_id  UUID         NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    expires_at               TIMESTAMPTZ  NOT NULL,
    max_uses                 INTEGER,
    use_count                INTEGER      NOT NULL DEFAULT 0,
    revoked_at               TIMESTAMPTZ,
    CONSTRAINT pk_messaging_channel_invites PRIMARY KEY (id),
    CONSTRAINT fk_messaging_channel_invites_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE,
    CONSTRAINT ux_messaging_channel_invites_token UNIQUE (token_sha256),
    CONSTRAINT ck_messaging_channel_invites_token CHECK (token_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_channel_invites_uses CHECK (max_uses IS NULL OR max_uses BETWEEN 1 AND 100000)
);
CREATE INDEX ix_messaging_channel_invites_channel ON messaging_channel_invites (channel_id, created_at DESC);

CREATE TABLE messaging_channel_scheduled_posts (
    id                   UUID         NOT NULL,
    channel_id           UUID         NOT NULL,
    author_principal_id  UUID         NOT NULL,
    kind                 VARCHAR(8)   NOT NULL,
    words                TEXT,
    media_id             UUID,
    link_preview         JSONB,
    poll                 JSONB,
    publish_at           TIMESTAMPTZ  NOT NULL,
    created_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_scheduled_posts PRIMARY KEY (id),
    CONSTRAINT fk_messaging_channel_scheduled_posts_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_channel_scheduled_posts_text CHECK (words IS NULL OR char_length(words) <= 4000)
);
CREATE INDEX ix_messaging_channel_scheduled_posts_due ON messaging_channel_scheduled_posts (publish_at);
CREATE INDEX ix_messaging_channel_scheduled_posts_channel ON messaging_channel_scheduled_posts (channel_id, publish_at);
CREATE INDEX ix_messaging_channel_scheduled_posts_media ON messaging_channel_scheduled_posts (media_id) WHERE media_id IS NOT NULL;

CREATE TABLE messaging_channel_polls (
    post_id    UUID         NOT NULL,
    question   TEXT         NOT NULL,
    multiple   BOOLEAN      NOT NULL,
    options    JSONB        NOT NULL,
    closes_at  TIMESTAMPTZ,
    CONSTRAINT pk_messaging_channel_polls PRIMARY KEY (post_id),
    CONSTRAINT fk_messaging_channel_polls_post FOREIGN KEY (post_id) REFERENCES messaging_channel_posts (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_channel_polls_question CHECK (char_length(question) BETWEEN 1 AND 300),
    CONSTRAINT ck_messaging_channel_polls_options CHECK (jsonb_typeof(options) = 'array' AND jsonb_array_length(options) BETWEEN 2 AND 12)
);

CREATE TABLE messaging_channel_poll_votes (
    post_id       UUID         NOT NULL,
    principal_id  UUID         NOT NULL,
    option_index  SMALLINT     NOT NULL,
    voted_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_poll_votes PRIMARY KEY (post_id, principal_id, option_index),
    CONSTRAINT fk_messaging_channel_poll_votes_poll FOREIGN KEY (post_id) REFERENCES messaging_channel_polls (post_id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_channel_poll_votes_option CHECK (option_index BETWEEN 0 AND 11)
);
CREATE INDEX ix_messaging_channel_poll_votes_time ON messaging_channel_poll_votes (post_id, voted_at);

CREATE TABLE messaging_channel_post_views (
    post_id       UUID         NOT NULL,
    principal_id  UUID         NOT NULL,
    viewed_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_post_views PRIMARY KEY (post_id, principal_id),
    CONSTRAINT fk_messaging_channel_post_views_post FOREIGN KEY (post_id) REFERENCES messaging_channel_posts (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_channel_post_views_time ON messaging_channel_post_views (post_id, viewed_at);

CREATE TABLE messaging_channel_follow_events (
    id           UUID         NOT NULL,
    channel_id   UUID         NOT NULL,
    kind         VARCHAR(8)   NOT NULL,
    occurred_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_follow_events PRIMARY KEY (id),
    CONSTRAINT fk_messaging_channel_follow_events_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_channel_follow_events_kind CHECK (kind IN ('FOLLOW', 'UNFOLLOW'))
);
CREATE INDEX ix_messaging_channel_follow_events_channel ON messaging_channel_follow_events (channel_id, occurred_at);

CREATE TABLE messaging_channel_forwards (
    id           UUID         NOT NULL,
    channel_id   UUID         NOT NULL,
    post_id      UUID         NOT NULL,
    recipients   INTEGER      NOT NULL,
    occurred_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_forwards PRIMARY KEY (id),
    CONSTRAINT fk_messaging_channel_forwards_post FOREIGN KEY (post_id) REFERENCES messaging_channel_posts (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_channel_forwards_recipients CHECK (recipients BETWEEN 1 AND 20)
);
CREATE INDEX ix_messaging_channel_forwards_channel ON messaging_channel_forwards (channel_id, occurred_at);
CREATE INDEX ix_messaging_channel_forwards_post ON messaging_channel_forwards (post_id);

CREATE TABLE messaging_channel_stars (
    principal_id  UUID         NOT NULL,
    post_id       UUID         NOT NULL,
    starred_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_stars PRIMARY KEY (principal_id, post_id),
    CONSTRAINT fk_messaging_channel_stars_post FOREIGN KEY (post_id) REFERENCES messaging_channel_posts (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_channel_stars_principal ON messaging_channel_stars (principal_id, starred_at DESC, post_id DESC);

-- Reports, verification applications and HQ moderation actions outlive the channel (no
-- foreign key): HQ keeps its review history after an owner deletes a channel.
CREATE TABLE messaging_channel_reports (
    id                     UUID         NOT NULL,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    channel_id             UUID         NOT NULL,
    post_id                UUID,
    reporter_principal_id  UUID         NOT NULL,
    category               VARCHAR(24)  NOT NULL,
    note                   TEXT,
    status                 VARCHAR(10)  NOT NULL DEFAULT 'OPEN',
    resolution             VARCHAR(48),
    resolved_at            TIMESTAMPTZ,
    created_at             TIMESTAMPTZ  NOT NULL,
    version                BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_messaging_channel_reports PRIMARY KEY (id),
    CONSTRAINT ck_messaging_channel_reports_status CHECK (status IN ('OPEN', 'ACTIONED', 'DISMISSED')),
    CONSTRAINT ck_messaging_channel_reports_note CHECK (note IS NULL OR char_length(note) <= 500)
);
CREATE UNIQUE INDEX ux_messaging_channel_reports_open ON messaging_channel_reports
    (channel_id, coalesce(post_id, '00000000-0000-0000-0000-000000000000'::uuid), reporter_principal_id) WHERE status = 'OPEN';
CREATE INDEX ix_messaging_channel_reports_queue ON messaging_channel_reports (status, created_at DESC, id DESC);
CREATE INDEX ix_messaging_channel_reports_channel ON messaging_channel_reports (channel_id, created_at DESC);

CREATE TABLE messaging_channel_verifications (
    id                      UUID         NOT NULL,
    tenant_id               UUID         NOT NULL,
    application_id          UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    channel_id              UUID         NOT NULL,
    applicant_principal_id  UUID         NOT NULL,
    basis                   VARCHAR(24)  NOT NULL,
    state                   VARCHAR(10)  NOT NULL DEFAULT 'PENDING',
    decision_reason         VARCHAR(48),
    decided_at              TIMESTAMPTZ,
    created_at              TIMESTAMPTZ  NOT NULL,
    version                 BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_messaging_channel_verifications PRIMARY KEY (id),
    CONSTRAINT ck_messaging_channel_verifications_state CHECK (state IN ('PENDING', 'APPROVED', 'REJECTED', 'REVOKED'))
);
CREATE UNIQUE INDEX ux_messaging_channel_verifications_pending ON messaging_channel_verifications (channel_id) WHERE state = 'PENDING';
CREATE UNIQUE INDEX ux_messaging_channel_verifications_approved ON messaging_channel_verifications (channel_id) WHERE state = 'APPROVED';
CREATE INDEX ix_messaging_channel_verifications_queue ON messaging_channel_verifications (state, created_at DESC, id DESC);
CREATE INDEX ix_messaging_channel_verifications_channel ON messaging_channel_verifications (channel_id, created_at DESC);

CREATE TABLE messaging_channel_moderation_actions (
    id               UUID          NOT NULL,
    tenant_id        UUID          NOT NULL,
    application_id   UUID          NOT NULL,
    environment_id   UUID          NOT NULL,
    channel_id       UUID          NOT NULL,
    post_id          UUID,
    report_id        UUID,
    verification_id  UUID,
    action           VARCHAR(24)   NOT NULL,
    resulting_state  VARCHAR(10),
    reason           VARCHAR(48)   NOT NULL,
    operator_ref     VARCHAR(140)  NOT NULL,
    created_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_channel_moderation_actions PRIMARY KEY (id)
);
CREATE INDEX ix_messaging_channel_moderation_actions_channel ON messaging_channel_moderation_actions (channel_id, created_at DESC);
CREATE INDEX ix_messaging_channel_moderation_actions_time ON messaging_channel_moderation_actions (created_at DESC, id DESC);

CREATE TABLE messaging_channel_audit (
    id                   UUID         NOT NULL,
    channel_id           UUID         NOT NULL,
    actor_principal_id   UUID         NOT NULL,
    action               VARCHAR(32)  NOT NULL,
    target_principal_id  UUID,
    target_id            UUID,
    occurred_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_audit PRIMARY KEY (id),
    CONSTRAINT fk_messaging_channel_audit_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_channel_audit_channel ON messaging_channel_audit (channel_id, occurred_at DESC, id DESC);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_channel_audit;
DROP TABLE IF EXISTS messaging_channel_moderation_actions;
DROP TABLE IF EXISTS messaging_channel_verifications;
DROP TABLE IF EXISTS messaging_channel_reports;
DROP TABLE IF EXISTS messaging_channel_stars;
DROP TABLE IF EXISTS messaging_channel_forwards;
DROP TABLE IF EXISTS messaging_channel_follow_events;
DROP TABLE IF EXISTS messaging_channel_post_views;
DROP TABLE IF EXISTS messaging_channel_poll_votes;
DROP TABLE IF EXISTS messaging_channel_polls;
DROP TABLE IF EXISTS messaging_channel_scheduled_posts;
DROP TABLE IF EXISTS messaging_channel_invites;
DROP TABLE IF EXISTS messaging_channel_admins;
DROP INDEX IF EXISTS ix_messaging_channel_reactions_time;
DROP INDEX IF EXISTS ix_messaging_channel_posts_pinned;
DELETE FROM messaging_channel_posts WHERE kind NOT IN ('TEXT', 'IMAGE', 'VIDEO', 'AUDIO');
ALTER TABLE messaging_channel_posts
    DROP CONSTRAINT IF EXISTS ck_messaging_channel_posts_kind,
    DROP COLUMN IF EXISTS edited_at,
    DROP COLUMN IF EXISTS pinned_at,
    DROP COLUMN IF EXISTS link_preview;
ALTER TABLE messaging_channel_posts ALTER COLUMN kind TYPE VARCHAR(5);
ALTER TABLE messaging_channel_posts ADD CONSTRAINT ck_messaging_channel_posts_kind CHECK (kind IN ('TEXT', 'IMAGE', 'VIDEO', 'AUDIO'));
DELETE FROM messaging_social_media WHERE kind = 'DOCUMENT';
ALTER TABLE messaging_social_media
    DROP CONSTRAINT IF EXISTS ck_messaging_social_media_kind,
    DROP COLUMN IF EXISTS file_name;
ALTER TABLE messaging_social_media ALTER COLUMN kind TYPE VARCHAR(5);
ALTER TABLE messaging_social_media ADD CONSTRAINT ck_messaging_social_media_kind CHECK (kind IN ('IMAGE', 'VIDEO', 'AUDIO'));
ALTER TABLE messaging_channel_followers DROP COLUMN IF EXISTS muted;
DROP INDEX IF EXISTS ix_messaging_channels_search;
DROP INDEX IF EXISTS ix_messaging_channels_filters;
DROP INDEX IF EXISTS ix_messaging_channels_verified;
DROP INDEX IF EXISTS ix_messaging_channels_new;
DROP INDEX IF EXISTS ix_messaging_channels_popular;
DROP INDEX IF EXISTS ux_messaging_channels_handle;
ALTER TABLE messaging_channels
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_handle,
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_category,
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_language,
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_country,
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_visibility,
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_moderation,
    DROP CONSTRAINT IF EXISTS ck_messaging_channels_version,
    DROP COLUMN IF EXISTS handle,
    DROP COLUMN IF EXISTS category,
    DROP COLUMN IF EXISTS language,
    DROP COLUMN IF EXISTS country,
    DROP COLUMN IF EXISTS visibility,
    DROP COLUMN IF EXISTS avatar_media_id,
    DROP COLUMN IF EXISTS verified_at,
    DROP COLUMN IF EXISTS moderation_state,
    DROP COLUMN IF EXISTS moderation_reason,
    DROP COLUMN IF EXISTS moderation_changed_at,
    DROP COLUMN IF EXISTS follower_count,
    DROP COLUMN IF EXISTS stats_since,
    DROP COLUMN IF EXISTS version;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
