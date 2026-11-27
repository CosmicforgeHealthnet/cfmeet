//! Status (24-hour updates) and Channels (ADR 0069; additive; the FIRST migration of the
//! separate `messaging_social_seaql_migrations` history, which no released image reads, so
//! a rollback to the previous image still boots on this schema).
//!
//! * `messaging_social_media` / `messaging_social_media_parts` — photos, videos and voice
//!   notes of statuses and channel posts. Bytes live in protected object storage, one object
//!   per part (`social/{environment}/{media}/{part}`), uploaded with signed single-object
//!   writes whose size and SHA-256 are fixed by the signature; the rows hold only the
//!   declared size / digest and when the part was seen stored. Media becomes usable only
//!   once COMPLETE (every part stored and the content recognised).
//! * `messaging_statuses` — a status with its audience SNAPSHOT (mode + the resolved
//!   principals at publish time); `messaging_status_views` (one row per viewer, any number
//!   of devices), `messaging_status_mentions`, `messaging_status_audience_settings` (the
//!   remembered default), `messaging_status_mutes`, `messaging_status_audience_lists`
//!   (close friends and named lists), `messaging_status_shares` and
//!   `messaging_status_notices` (the Notification Service outbox of mentions and shares,
//!   written only when enabled).
//! * `messaging_channels`, `messaging_channel_followers`, `messaging_channel_posts`,
//!   `messaging_channel_reactions` — broadcast channels (owner-only posting).
//!
//! No column holds message content of a conversation, a key or E2EE material. Principals,
//! tenants, applications and environments are opaque Platform Access ids.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_social_media (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    owner_principal_id  UUID         NOT NULL,
    purpose             VARCHAR(8)   NOT NULL,
    kind                VARCHAR(5)   NOT NULL,
    media_type          VARCHAR(64)  NOT NULL,
    size_bytes          BIGINT       NOT NULL,
    part_bytes          INTEGER      NOT NULL,
    part_count          INTEGER      NOT NULL,
    duration_ms         INTEGER,
    state               VARCHAR(9)   NOT NULL DEFAULT 'UPLOADING',
    created_at          TIMESTAMPTZ  NOT NULL,
    completed_at        TIMESTAMPTZ,
    CONSTRAINT pk_messaging_social_media PRIMARY KEY (id),
    CONSTRAINT ck_messaging_social_media_purpose CHECK (purpose IN ('STATUS', 'CHANNEL')),
    CONSTRAINT ck_messaging_social_media_kind CHECK (kind IN ('IMAGE', 'VIDEO', 'AUDIO')),
    CONSTRAINT ck_messaging_social_media_size CHECK (size_bytes > 0 AND size_bytes <= 67108864),
    CONSTRAINT ck_messaging_social_media_parts CHECK (part_bytes > 0 AND part_count BETWEEN 1 AND 64),
    CONSTRAINT ck_messaging_social_media_duration CHECK (duration_ms IS NULL OR duration_ms BETWEEN 0 AND 3600000),
    CONSTRAINT ck_messaging_social_media_state CHECK (state IN ('UPLOADING', 'COMPLETE')),
    CONSTRAINT ck_messaging_social_media_completed CHECK ((state = 'COMPLETE') = (completed_at IS NOT NULL))
);
CREATE INDEX ix_messaging_social_media_owner ON messaging_social_media (environment_id, owner_principal_id, created_at);
CREATE INDEX ix_messaging_social_media_created ON messaging_social_media (created_at);

CREATE TABLE messaging_social_media_parts (
    media_id     UUID         NOT NULL,
    part         INTEGER      NOT NULL,
    size_bytes   INTEGER      NOT NULL,
    sha256       CHAR(64)     NOT NULL,
    declared_at  TIMESTAMPTZ  NOT NULL,
    stored_at    TIMESTAMPTZ,
    CONSTRAINT pk_messaging_social_media_parts PRIMARY KEY (media_id, part),
    CONSTRAINT fk_messaging_social_media_parts_media FOREIGN KEY (media_id) REFERENCES messaging_social_media (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_social_media_parts_part CHECK (part >= 0 AND part < 64),
    CONSTRAINT ck_messaging_social_media_parts_size CHECK (size_bytes > 0),
    CONSTRAINT ck_messaging_social_media_parts_digest CHECK (sha256 ~ '^[0-9a-f]{64}$')
);

CREATE TABLE messaging_statuses (
    id                    UUID          NOT NULL,
    tenant_id             UUID          NOT NULL,
    application_id        UUID          NOT NULL,
    environment_id        UUID          NOT NULL,
    author_principal_id   UUID          NOT NULL,
    kind                  VARCHAR(5)    NOT NULL,
    words             TEXT,
    caption               TEXT,
    style                 JSONB         NOT NULL DEFAULT '{}'::jsonb,
    media_id              UUID,
    start_ms              INTEGER,
    end_ms                INTEGER,
    music_media_id        UUID,
    music_start_ms        INTEGER,
    music_end_ms          INTEGER,
    music_title           TEXT,
    group_id              UUID,
    sequence              INTEGER,
    audience_mode         VARCHAR(15)   NOT NULL,
    audience_list_id      UUID,
    audience_label        TEXT,
    audience_principals   UUID[]        NOT NULL DEFAULT '{}',
    excluded_count        INTEGER       NOT NULL DEFAULT 0,
    audience_as_of        TIMESTAMPTZ,
    allow_sharing         BOOLEAN       NOT NULL DEFAULT true,
    origin_status_id      UUID,
    origin_author_principal_id UUID,
    created_at            TIMESTAMPTZ   NOT NULL,
    expires_at            TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_statuses PRIMARY KEY (id),
    CONSTRAINT ck_messaging_statuses_kind CHECK (kind IN ('TEXT', 'IMAGE', 'VIDEO', 'AUDIO')),
    CONSTRAINT ck_messaging_statuses_text CHECK (words IS NULL OR char_length(words) <= 700),
    CONSTRAINT ck_messaging_statuses_caption CHECK (caption IS NULL OR char_length(caption) <= 700),
    CONSTRAINT ck_messaging_statuses_music_title CHECK (music_title IS NULL OR char_length(music_title) <= 120),
    CONSTRAINT ck_messaging_statuses_style CHECK (jsonb_typeof(style) = 'object' AND octet_length(style::text) <= 1024),
    CONSTRAINT ck_messaging_statuses_audience CHECK (audience_mode IN ('CONTACTS', 'CONTACTS_EXCEPT', 'ONLY', 'CLOSE_FRIENDS', 'CUSTOM')),
    CONSTRAINT ck_messaging_statuses_audience_label CHECK (audience_label IS NULL OR char_length(audience_label) <= 60),
    CONSTRAINT ck_messaging_statuses_origin CHECK ((origin_status_id IS NULL) = (origin_author_principal_id IS NULL)),
    CONSTRAINT ck_messaging_statuses_audience_size CHECK (cardinality(audience_principals) <= 5000),
    CONSTRAINT ck_messaging_statuses_sequence CHECK (sequence IS NULL OR sequence BETWEEN 0 AND 999),
    CONSTRAINT ck_messaging_statuses_expiry CHECK (expires_at > created_at)
);
CREATE INDEX ix_messaging_statuses_author ON messaging_statuses (environment_id, author_principal_id, expires_at);
CREATE INDEX ix_messaging_statuses_expiry ON messaging_statuses (expires_at);
CREATE INDEX ix_messaging_statuses_media ON messaging_statuses (media_id) WHERE media_id IS NOT NULL;
CREATE INDEX ix_messaging_statuses_origin ON messaging_statuses (origin_status_id) WHERE origin_status_id IS NOT NULL;
CREATE INDEX ix_messaging_statuses_group ON messaging_statuses (environment_id, author_principal_id, group_id) WHERE group_id IS NOT NULL;
CREATE INDEX ix_messaging_statuses_music ON messaging_statuses (music_media_id) WHERE music_media_id IS NOT NULL;

CREATE TABLE messaging_status_views (
    status_id            UUID         NOT NULL,
    viewer_principal_id  UUID         NOT NULL,
    viewed_at            TIMESTAMPTZ  NOT NULL,
    liked                BOOLEAN      NOT NULL DEFAULT false,
    reaction             VARCHAR(64),
    CONSTRAINT pk_messaging_status_views PRIMARY KEY (status_id, viewer_principal_id),
    CONSTRAINT fk_messaging_status_views_status FOREIGN KEY (status_id) REFERENCES messaging_statuses (id) ON DELETE CASCADE
);

CREATE TABLE messaging_status_mentions (
    status_id     UUID  NOT NULL,
    principal_id  UUID  NOT NULL,
    CONSTRAINT pk_messaging_status_mentions PRIMARY KEY (status_id, principal_id),
    CONSTRAINT fk_messaging_status_mentions_status FOREIGN KEY (status_id) REFERENCES messaging_statuses (id) ON DELETE CASCADE
);

CREATE TABLE messaging_status_audience_settings (
    environment_id  UUID         NOT NULL,
    principal_id    UUID         NOT NULL,
    mode            VARCHAR(15)  NOT NULL,
    principals      UUID[]       NOT NULL DEFAULT '{}',
    list_id         UUID,
    allow_sharing   BOOLEAN      NOT NULL DEFAULT true,
    updated_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_status_audience_settings PRIMARY KEY (environment_id, principal_id),
    CONSTRAINT ck_messaging_status_audience_settings_mode CHECK (mode IN ('CONTACTS', 'CONTACTS_EXCEPT', 'ONLY', 'CLOSE_FRIENDS', 'CUSTOM')),
    CONSTRAINT ck_messaging_status_audience_settings_size CHECK (cardinality(principals) <= 1000)
);

CREATE TABLE messaging_status_audience_lists (
    id              UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    principal_id    UUID         NOT NULL,
    kind            VARCHAR(13)  NOT NULL,
    name            TEXT         NOT NULL,
    members         UUID[]       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ  NOT NULL,
    updated_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_status_audience_lists PRIMARY KEY (id),
    CONSTRAINT ck_messaging_status_audience_lists_kind CHECK (kind IN ('CLOSE_FRIENDS', 'CUSTOM')),
    CONSTRAINT ck_messaging_status_audience_lists_name CHECK (char_length(name) BETWEEN 1 AND 60),
    CONSTRAINT ck_messaging_status_audience_lists_size CHECK (cardinality(members) <= 1000)
);
CREATE INDEX ix_messaging_status_audience_lists_owner ON messaging_status_audience_lists (environment_id, principal_id);
CREATE UNIQUE INDEX uq_messaging_status_close_friends ON messaging_status_audience_lists (environment_id, principal_id) WHERE kind = 'CLOSE_FRIENDS';

CREATE TABLE messaging_status_shares (
    id                    UUID         NOT NULL,
    status_id             UUID         NOT NULL,
    sharer_principal_id   UUID         NOT NULL,
    kind                  VARCHAR(7)   NOT NULL,
    target_principal_id   UUID,
    reshare_status_id     UUID,
    created_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_status_shares PRIMARY KEY (id),
    CONSTRAINT fk_messaging_status_shares_status FOREIGN KEY (status_id) REFERENCES messaging_statuses (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_status_shares_kind CHECK (kind IN ('FORWARD', 'RESHARE'))
);
CREATE INDEX ix_messaging_status_shares_status ON messaging_status_shares (status_id);

CREATE TABLE messaging_status_mutes (
    environment_id        UUID         NOT NULL,
    principal_id          UUID         NOT NULL,
    muted_principal_id    UUID         NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_status_mutes PRIMARY KEY (environment_id, principal_id, muted_principal_id),
    CONSTRAINT ck_messaging_status_mutes_distinct CHECK (principal_id <> muted_principal_id)
);

CREATE TABLE messaging_status_notices (
    id                  UUID         NOT NULL,
    event_id            VARCHAR(64)  NOT NULL,
    kind                VARCHAR(7)   NOT NULL,
    subject_id          UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    status_id           UUID         NOT NULL,
    principal_id        UUID         NOT NULL,
    service_account_id  UUID         NOT NULL,
    environment_kind    VARCHAR(16)  NOT NULL,
    first_party         BOOLEAN      NOT NULL,
    envelope            JSONB,
    state               VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    outcome             VARCHAR(9),
    last_error          VARCHAR(64),
    attempts            INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ  NOT NULL,
    occurred_at         TIMESTAMPTZ  NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    completed_at        TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_status_notices PRIMARY KEY (id),
    CONSTRAINT uq_messaging_status_notices_event UNIQUE (event_id),
    CONSTRAINT uq_messaging_status_notices_target UNIQUE (kind, subject_id, principal_id),
    CONSTRAINT ck_messaging_status_notices_kind CHECK (kind IN ('MENTION', 'SHARE')),
    CONSTRAINT ck_messaging_status_notices_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_status_notices_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_messaging_status_notices_done CHECK ((state = 'PENDING') = (completed_at IS NULL))
);
CREATE INDEX ix_messaging_status_notices_due ON messaging_status_notices (next_attempt_at) WHERE state = 'PENDING';

CREATE TABLE messaging_channels (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    owner_principal_id  UUID         NOT NULL,
    name                TEXT         NOT NULL,
    description         TEXT,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channels PRIMARY KEY (id),
    CONSTRAINT ck_messaging_channels_name CHECK (char_length(name) BETWEEN 1 AND 80),
    CONSTRAINT ck_messaging_channels_description CHECK (description IS NULL OR char_length(description) <= 500)
);
CREATE INDEX ix_messaging_channels_owner ON messaging_channels (environment_id, owner_principal_id);
CREATE INDEX ix_messaging_channels_environment ON messaging_channels (environment_id, created_at);

CREATE TABLE messaging_channel_followers (
    channel_id    UUID         NOT NULL,
    principal_id  UUID         NOT NULL,
    followed_at   TIMESTAMPTZ  NOT NULL,
    last_read_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_followers PRIMARY KEY (channel_id, principal_id),
    CONSTRAINT fk_messaging_channel_followers_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_channel_followers_principal ON messaging_channel_followers (principal_id);

CREATE TABLE messaging_channel_posts (
    id                   UUID         NOT NULL,
    channel_id           UUID         NOT NULL,
    author_principal_id  UUID         NOT NULL,
    kind                 VARCHAR(5)   NOT NULL,
    words            TEXT,
    media_id             UUID,
    created_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_posts PRIMARY KEY (id),
    CONSTRAINT fk_messaging_channel_posts_channel FOREIGN KEY (channel_id) REFERENCES messaging_channels (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_channel_posts_kind CHECK (kind IN ('TEXT', 'IMAGE', 'VIDEO', 'AUDIO')),
    CONSTRAINT ck_messaging_channel_posts_text CHECK (words IS NULL OR char_length(words) <= 4000)
);
CREATE INDEX ix_messaging_channel_posts_channel ON messaging_channel_posts (channel_id, created_at DESC);
CREATE INDEX ix_messaging_channel_posts_media ON messaging_channel_posts (media_id) WHERE media_id IS NOT NULL;

CREATE TABLE messaging_channel_reactions (
    post_id       UUID         NOT NULL,
    principal_id  UUID         NOT NULL,
    emoji         VARCHAR(64)  NOT NULL,
    reacted_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_channel_reactions PRIMARY KEY (post_id, principal_id),
    CONSTRAINT fk_messaging_channel_reactions_post FOREIGN KEY (post_id) REFERENCES messaging_channel_posts (id) ON DELETE CASCADE
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_channel_reactions;
DROP TABLE IF EXISTS messaging_channel_posts;
DROP TABLE IF EXISTS messaging_channel_followers;
DROP TABLE IF EXISTS messaging_channels;
DROP TABLE IF EXISTS messaging_status_notices;
DROP TABLE IF EXISTS messaging_status_mutes;
DROP TABLE IF EXISTS messaging_status_shares;
DROP TABLE IF EXISTS messaging_status_audience_lists;
DROP TABLE IF EXISTS messaging_status_audience_settings;
DROP TABLE IF EXISTS messaging_status_mentions;
DROP TABLE IF EXISTS messaging_status_views;
DROP TABLE IF EXISTS messaging_statuses;
DROP TABLE IF EXISTS messaging_social_media_parts;
DROP TABLE IF EXISTS messaging_social_media;
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
