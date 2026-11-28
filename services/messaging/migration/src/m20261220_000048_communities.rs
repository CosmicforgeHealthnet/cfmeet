//! Communities (ADR 0073; additive; the FIRST migration of the separate
//! `messaging_communities_seaql_migrations` history, which no released image reads, so a
//! rollback to the previous image still boots on this schema).
//!
//! * `messaging_communities` — one community: name, description, rules, privacy, the
//!   group-creation permission, its owner and its Announcements group (an ordinary GROUP
//!   conversation).
//! * `messaging_community_members` — community membership and role (OWNER / ADMIN / MEMBER).
//! * `messaging_community_groups` — the group conversations of a community (a conversation
//!   belongs to at most one community), their order and access (OPEN / RESTRICTED).
//! * `messaging_community_invitations`, `messaging_community_invite_links` (SHA-256 of the
//!   token only), `messaging_community_join_requests` (community membership or a restricted
//!   group), `messaging_community_bans`.
//! * `messaging_community_preferences` — a person's chat-list mode (TOGETHER / SEPARATE).
//! * `messaging_community_notices` — the Notification Service outbox of invitations, join
//!   requests and approvals (written only when enabled).
//! * Conversation members admitted through a community record `authority_source =
//!   'COMMUNITY'` (the previous image only reads the column).
//!
//! No column holds message content, a key or E2EE material. Principals, tenants,
//! applications and environments are opaque Platform Access ids.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_authority,
    ADD CONSTRAINT ck_messaging_conversation_members_authority
        CHECK (authority_source IN ('API', 'INVITATION', 'MEETING', 'INVITE_LINK', 'ORGANIZATION_TEAM', 'COMMUNITY'));

CREATE TABLE messaging_communities (
    id                              UUID          NOT NULL,
    tenant_id                       UUID          NOT NULL,
    application_id                  UUID          NOT NULL,
    environment_id                  UUID          NOT NULL,
    owner_principal_id              UUID          NOT NULL,
    name                            TEXT          NOT NULL,
    description                     TEXT,
    rules                           TEXT,
    privacy                         VARCHAR(11)   NOT NULL DEFAULT 'APPROVAL',
    members_can_add_groups          BOOLEAN       NOT NULL DEFAULT false,
    announcements_conversation_id   UUID,
    state                           VARCHAR(7)    NOT NULL DEFAULT 'ACTIVE',
    version                         INTEGER       NOT NULL DEFAULT 1,
    created_by_principal_id         UUID          NOT NULL,
    created_at                      TIMESTAMPTZ   NOT NULL,
    updated_at                      TIMESTAMPTZ   NOT NULL,
    deleted_at                      TIMESTAMPTZ,
    CONSTRAINT pk_messaging_communities PRIMARY KEY (id),
    CONSTRAINT ck_messaging_communities_name CHECK (char_length(name) BETWEEN 1 AND 100),
    CONSTRAINT ck_messaging_communities_description CHECK (description IS NULL OR char_length(description) <= 2048),
    CONSTRAINT ck_messaging_communities_rules CHECK (rules IS NULL OR char_length(rules) <= 4000),
    CONSTRAINT ck_messaging_communities_privacy CHECK (privacy IN ('OPEN', 'APPROVAL', 'INVITE_ONLY')),
    CONSTRAINT ck_messaging_communities_state CHECK (state IN ('ACTIVE', 'DELETED')),
    CONSTRAINT ck_messaging_communities_deleted CHECK ((state = 'DELETED') = (deleted_at IS NOT NULL))
);
CREATE INDEX ix_messaging_communities_owner ON messaging_communities (environment_id, owner_principal_id) WHERE state = 'ACTIVE';
CREATE UNIQUE INDEX uq_messaging_communities_announcements ON messaging_communities (announcements_conversation_id)
    WHERE announcements_conversation_id IS NOT NULL;

CREATE TABLE messaging_community_members (
    community_id          UUID          NOT NULL,
    principal_id          UUID          NOT NULL,
    environment_id        UUID          NOT NULL,
    role                  VARCHAR(6)    NOT NULL,
    state                 VARCHAR(7)    NOT NULL DEFAULT 'ACTIVE',
    source                VARCHAR(10)   NOT NULL,
    added_by_principal_id UUID,
    joined_at             TIMESTAMPTZ   NOT NULL,
    updated_at            TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_community_members PRIMARY KEY (community_id, principal_id),
    CONSTRAINT fk_messaging_community_members_community FOREIGN KEY (community_id) REFERENCES messaging_communities (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_community_members_role CHECK (role IN ('OWNER', 'ADMIN', 'MEMBER')),
    CONSTRAINT ck_messaging_community_members_state CHECK (state IN ('ACTIVE', 'LEFT', 'REMOVED')),
    CONSTRAINT ck_messaging_community_members_source CHECK (source IN ('CREATOR', 'INVITATION', 'LINK', 'REQUEST'))
);
CREATE INDEX ix_messaging_community_members_principal ON messaging_community_members (environment_id, principal_id, joined_at DESC) WHERE state = 'ACTIVE';
CREATE INDEX ix_messaging_community_members_page ON messaging_community_members (community_id, joined_at, principal_id) WHERE state = 'ACTIVE';
CREATE UNIQUE INDEX uq_messaging_community_members_owner ON messaging_community_members (community_id) WHERE role = 'OWNER' AND state = 'ACTIVE';

CREATE TABLE messaging_community_groups (
    community_id           UUID          NOT NULL,
    conversation_id        UUID          NOT NULL,
    position               INTEGER       NOT NULL,
    access                 VARCHAR(10)   NOT NULL DEFAULT 'OPEN',
    announcements          BOOLEAN       NOT NULL DEFAULT false,
    linked_by_principal_id UUID          NOT NULL,
    linked_at              TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_community_groups PRIMARY KEY (community_id, conversation_id),
    CONSTRAINT uq_messaging_community_groups_conversation UNIQUE (conversation_id),
    CONSTRAINT fk_messaging_community_groups_community FOREIGN KEY (community_id) REFERENCES messaging_communities (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_community_groups_access CHECK (access IN ('OPEN', 'RESTRICTED'))
);
CREATE INDEX ix_messaging_community_groups_order ON messaging_community_groups (community_id, position);

CREATE TABLE messaging_community_invitations (
    id                       UUID          NOT NULL,
    community_id             UUID          NOT NULL,
    environment_id           UUID          NOT NULL,
    principal_id             UUID          NOT NULL,
    invited_by_principal_id  UUID          NOT NULL,
    by_admin                 BOOLEAN       NOT NULL,
    state                    VARCHAR(8)    NOT NULL DEFAULT 'PENDING',
    created_at               TIMESTAMPTZ   NOT NULL,
    expires_at               TIMESTAMPTZ   NOT NULL,
    answered_at              TIMESTAMPTZ,
    CONSTRAINT pk_messaging_community_invitations PRIMARY KEY (id),
    CONSTRAINT fk_messaging_community_invitations_community FOREIGN KEY (community_id) REFERENCES messaging_communities (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_community_invitations_state CHECK (state IN ('PENDING', 'ACCEPTED', 'DECLINED', 'REVOKED'))
);
CREATE UNIQUE INDEX uq_messaging_community_invitations_pending ON messaging_community_invitations (community_id, principal_id) WHERE state = 'PENDING';
CREATE INDEX ix_messaging_community_invitations_principal ON messaging_community_invitations (environment_id, principal_id, created_at DESC) WHERE state = 'PENDING';

CREATE TABLE messaging_community_invite_links (
    id                       UUID          NOT NULL,
    community_id             UUID          NOT NULL,
    environment_id           UUID          NOT NULL,
    token_sha256             VARCHAR(64)   NOT NULL,
    max_uses                 INTEGER,
    use_count                INTEGER       NOT NULL DEFAULT 0,
    expires_at               TIMESTAMPTZ   NOT NULL,
    created_by_principal_id  UUID          NOT NULL,
    created_at               TIMESTAMPTZ   NOT NULL,
    revoked_at               TIMESTAMPTZ,
    revoked_by_principal_id  UUID,
    CONSTRAINT pk_messaging_community_invite_links PRIMARY KEY (id),
    CONSTRAINT uq_messaging_community_invite_links_token UNIQUE (token_sha256),
    CONSTRAINT fk_messaging_community_invite_links_community FOREIGN KEY (community_id) REFERENCES messaging_communities (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_community_invite_links_uses CHECK (max_uses IS NULL OR max_uses BETWEEN 1 AND 10000)
);
CREATE INDEX ix_messaging_community_invite_links_community ON messaging_community_invite_links (community_id, created_at DESC);

CREATE TABLE messaging_community_join_requests (
    id                       UUID          NOT NULL,
    community_id             UUID          NOT NULL,
    environment_id           UUID          NOT NULL,
    principal_id             UUID          NOT NULL,
    conversation_id          UUID,
    state                    VARCHAR(9)    NOT NULL DEFAULT 'PENDING',
    message                  TEXT,
    via                      VARCHAR(10)   NOT NULL,
    created_at               TIMESTAMPTZ   NOT NULL,
    decided_at               TIMESTAMPTZ,
    decided_by_principal_id  UUID,
    CONSTRAINT pk_messaging_community_join_requests PRIMARY KEY (id),
    CONSTRAINT fk_messaging_community_join_requests_community FOREIGN KEY (community_id) REFERENCES messaging_communities (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_community_join_requests_state CHECK (state IN ('PENDING', 'APPROVED', 'REJECTED', 'CANCELLED')),
    CONSTRAINT ck_messaging_community_join_requests_via CHECK (via IN ('LINK', 'INVITATION', 'GROUP')),
    CONSTRAINT ck_messaging_community_join_requests_message CHECK (message IS NULL OR char_length(message) <= 500),
    CONSTRAINT ck_messaging_community_join_requests_decided CHECK ((state = 'PENDING') = (decided_at IS NULL))
);
CREATE UNIQUE INDEX uq_messaging_community_join_requests_pending ON messaging_community_join_requests
    (community_id, principal_id, COALESCE(conversation_id, '00000000-0000-0000-0000-000000000000'::uuid)) WHERE state = 'PENDING';
CREATE INDEX ix_messaging_community_join_requests_queue ON messaging_community_join_requests (community_id, state, created_at, id);
CREATE INDEX ix_messaging_community_join_requests_principal ON messaging_community_join_requests (environment_id, principal_id, created_at DESC);

CREATE TABLE messaging_community_bans (
    community_id           UUID          NOT NULL,
    principal_id           UUID          NOT NULL,
    banned_by_principal_id UUID          NOT NULL,
    created_at             TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_community_bans PRIMARY KEY (community_id, principal_id),
    CONSTRAINT fk_messaging_community_bans_community FOREIGN KEY (community_id) REFERENCES messaging_communities (id) ON DELETE CASCADE
);

CREATE TABLE messaging_community_preferences (
    environment_id   UUID          NOT NULL,
    principal_id     UUID          NOT NULL,
    chat_list_mode   VARCHAR(8)    NOT NULL DEFAULT 'TOGETHER',
    updated_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_community_preferences PRIMARY KEY (environment_id, principal_id),
    CONSTRAINT ck_messaging_community_preferences_mode CHECK (chat_list_mode IN ('TOGETHER', 'SEPARATE'))
);

CREATE TABLE messaging_community_notices (
    id                  UUID          NOT NULL,
    event_id            VARCHAR(64)   NOT NULL,
    kind                VARCHAR(16)   NOT NULL,
    subject_id          UUID          NOT NULL,
    tenant_id           UUID          NOT NULL,
    application_id      UUID          NOT NULL,
    environment_id      UUID          NOT NULL,
    community_id        UUID          NOT NULL,
    principal_id        UUID          NOT NULL,
    service_account_id  UUID          NOT NULL,
    environment_kind    VARCHAR(16)   NOT NULL,
    first_party         BOOLEAN       NOT NULL,
    envelope            JSONB,
    state               VARCHAR(8)    NOT NULL DEFAULT 'PENDING',
    outcome             VARCHAR(9),
    last_error          VARCHAR(64),
    attempts            INTEGER       NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ   NOT NULL,
    occurred_at         TIMESTAMPTZ   NOT NULL,
    created_at          TIMESTAMPTZ   NOT NULL,
    completed_at        TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_community_notices PRIMARY KEY (id),
    CONSTRAINT uq_messaging_community_notices_event UNIQUE (event_id),
    CONSTRAINT uq_messaging_community_notices_subject UNIQUE (kind, subject_id, principal_id),
    CONSTRAINT ck_messaging_community_notices_kind CHECK (kind IN ('INVITED', 'JOIN_REQUESTED', 'APPROVED')),
    CONSTRAINT ck_messaging_community_notices_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_community_notices_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_messaging_community_notices_done CHECK ((state = 'PENDING') = (completed_at IS NULL))
);
CREATE INDEX ix_messaging_community_notices_due ON messaging_community_notices (next_attempt_at) WHERE state = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_community_notices;
DROP TABLE IF EXISTS messaging_community_preferences;
DROP TABLE IF EXISTS messaging_community_bans;
DROP TABLE IF EXISTS messaging_community_join_requests;
DROP TABLE IF EXISTS messaging_community_invite_links;
DROP TABLE IF EXISTS messaging_community_invitations;
DROP TABLE IF EXISTS messaging_community_groups;
DROP TABLE IF EXISTS messaging_community_members;
DROP TABLE IF EXISTS messaging_communities;
ALTER TABLE messaging_conversation_members
    DROP CONSTRAINT ck_messaging_conversation_members_authority,
    ADD CONSTRAINT ck_messaging_conversation_members_authority
        CHECK (authority_source IN ('API', 'INVITATION', 'MEETING', 'INVITE_LINK', 'ORGANIZATION_TEAM')) NOT VALID;
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
