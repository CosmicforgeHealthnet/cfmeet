//! ADR 0062 (CF Meet Platform "Build") — partner users join meetings with their CF Meet
//! user token, and partner meeting chat.
//!
//! * `meeting_platform_participants`: the logical participant a Platform Access principal
//!   (a partner user) holds in a conference. One per (conference, principal), so a re-join
//!   — from the same or another device, with a fresh user token — keeps the same
//!   participant (role history, admission, bans). The user token of the latest join and
//!   its minting credential / service account (`crd` / `sva`) are recorded so derived
//!   credentials can be traced to the partner key that minted them.
//! * `meeting_platform_chat_links` / `meeting_platform_chat_members`: the MANAGED Messaging
//!   conversation of an environment-owned conference, owned by the environment's meeting
//!   system principal and synchronised in the partner environment's own scope, and the
//!   last membership state applied per participant (`MEETING_PLATFORM_CHAT_ENABLED`).
//!
//! Additive only: new tables in a SEPARATE history (`meeting_platform_build_migrations`),
//! so the previous image still starts after it is applied. Down refuses while partner
//! participants exist (no data is silently dropped).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS meeting_platform_participants (
    conference_id       UUID NOT NULL REFERENCES conference_sessions (id) ON DELETE CASCADE,
    principal_id        UUID NOT NULL,
    participant_id      UUID NOT NULL REFERENCES conference_participants (id) ON DELETE CASCADE,
    environment_id      UUID NOT NULL,
    environment_kind    TEXT NOT NULL
        CONSTRAINT ck_meeting_platform_participants_kind
        CHECK (environment_kind IN ('SANDBOX','TEST','PRODUCTION')),
    token_id            UUID NOT NULL,
    credential_id       UUID NOT NULL,
    service_account_id  UUID NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_joined_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_meeting_platform_participants PRIMARY KEY (conference_id, principal_id),
    CONSTRAINT uq_meeting_platform_participants_participant UNIQUE (participant_id)
);
CREATE INDEX IF NOT EXISTS idx_meeting_platform_participants_principal
    ON meeting_platform_participants (environment_id, principal_id);
CREATE INDEX IF NOT EXISTS idx_meeting_platform_participants_credential
    ON meeting_platform_participants (credential_id);

CREATE TABLE IF NOT EXISTS meeting_platform_chat_links (
    conference_id           UUID PRIMARY KEY REFERENCES conference_sessions (id) ON DELETE CASCADE,
    tenant_id               UUID NOT NULL,
    application_id          UUID NOT NULL,
    environment_id          UUID NOT NULL,
    environment_kind        TEXT NOT NULL
        CONSTRAINT ck_meeting_platform_chat_links_kind
        CHECK (environment_kind IN ('SANDBOX','TEST','PRODUCTION')),
    service_account_id      UUID NOT NULL,
    conversation_id         UUID NOT NULL,
    owner_principal_id      UUID NOT NULL,
    lifecycle_synced_state  TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS meeting_platform_chat_members (
    participant_id  UUID PRIMARY KEY REFERENCES conference_participants (id) ON DELETE CASCADE,
    conference_id   UUID NOT NULL REFERENCES meeting_platform_chat_links (conference_id) ON DELETE CASCADE,
    principal_id    UUID NOT NULL,
    synced_state    TEXT NOT NULL
        CONSTRAINT ck_meeting_platform_chat_members_state
        CHECK (synced_state IN ('ACTIVE','REMOVED','BANNED')),
    synced_role     TEXT,
    synced_version  BIGINT NOT NULL,
    synced_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE SEQUENCE IF NOT EXISTS meeting_platform_chat_sync_version_seq;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM meeting_platform_participants) THEN
        RAISE EXCEPTION 'partner user participants exist; refusing to downgrade';
    END IF;
END $$;
DROP SEQUENCE IF EXISTS meeting_platform_chat_sync_version_seq;
DROP TABLE IF EXISTS meeting_platform_chat_members;
DROP TABLE IF EXISTS meeting_platform_chat_links;
DROP TABLE IF EXISTS meeting_platform_participants;
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
