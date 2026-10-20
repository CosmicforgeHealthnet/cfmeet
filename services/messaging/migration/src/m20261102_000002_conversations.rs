//! The Conversation domain (Part V Workstream D).
//!
//! * `messaging_application_policies` — an application environment's messaging policy
//!   overrides (versioned; optimistic concurrency; audited by the service).
//! * `messaging_policy_snapshots` — immutable, content-addressed EFFECTIVE policies
//!   (platform defaults merged with the overrides) that conversations reference, so every
//!   conversation's policy can be reproduced and audited.
//! * `messaging_conversations` — the canonical Conversation aggregate: authoritative
//!   tenant / application / environment, one of eight generic context classes, an optional
//!   opaque context reference, the PERSISTED security profile with its protocol / cipher
//!   suite / MLS group binding, lifecycle state, policy snapshot and aggregate version. A
//!   trigger makes identity, context and security profile immutable (no silent
//!   PRIVATE_E2EE → MANAGED), enforces the lifecycle transitions and the one-step version.
//! * `messaging_conversation_members` — only the initial owner / counterpart association
//!   (full membership is Workstream E).
//! * `messaging_conversation_sequences` — the transactional per-conversation message
//!   sequence allocator (kept apart from the aggregate row: allocation never contends with
//!   lifecycle updates).
//! * `messaging_idempotency_keys` — idempotent creation responses.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_application_policies (
    environment_id   UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    version          BIGINT       NOT NULL,
    document         JSONB        NOT NULL,
    updated_by_credential_id UUID NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_application_policies PRIMARY KEY (environment_id),
    CONSTRAINT ck_messaging_application_policies_version CHECK (version >= 1),
    CONSTRAINT ck_messaging_application_policies_document CHECK (jsonb_typeof(document) = 'object'
        AND octet_length(document::text) <= 16384)
);

CREATE TABLE messaging_policy_snapshots (
    id                      UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    digest                  CHAR(64)     NOT NULL,
    platform_policy_version INTEGER      NOT NULL,
    application_policy_version BIGINT    NOT NULL,
    document                JSONB        NOT NULL,
    created_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_policy_snapshots PRIMARY KEY (id),
    CONSTRAINT uq_messaging_policy_snapshots_digest UNIQUE (environment_id, digest),
    CONSTRAINT uq_messaging_policy_snapshots_scope UNIQUE (environment_id, id),
    CONSTRAINT ck_messaging_policy_snapshots_digest CHECK (digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_policy_snapshots_versions CHECK (platform_policy_version >= 1 AND application_policy_version >= 0),
    CONSTRAINT ck_messaging_policy_snapshots_document CHECK (jsonb_typeof(document) = 'object'
        AND octet_length(document::text) <= 32768)
);
CREATE TRIGGER trg_messaging_policy_snapshots_immutable BEFORE UPDATE OR DELETE ON messaging_policy_snapshots
    FOR EACH ROW EXECUTE FUNCTION messaging_forbid_mutation();

CREATE TABLE messaging_conversations (
    id                       UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    context_class            VARCHAR(24)  NOT NULL,
    context_namespace        VARCHAR(64),
    context_ref              VARCHAR(256),
    context_ref_version      INTEGER,
    security_profile         VARCHAR(16)  NOT NULL,
    protocol                 VARCHAR(24)  NOT NULL,
    ciphersuite              VARCHAR(64),
    mls_group_id             BYTEA,
    security_policy_version  INTEGER      NOT NULL,
    lifecycle_state          VARCHAR(16)  NOT NULL,
    lifecycle_reason         VARCHAR(48),
    policy_snapshot_id       UUID         NOT NULL,
    policy_version           BIGINT       NOT NULL,
    version                  BIGINT       NOT NULL DEFAULT 1,
    created_by_principal_id  UUID         NOT NULL,
    created_by_credential_id UUID         NOT NULL,
    owner_principal_id       UUID         NOT NULL,
    direct_pair_key          CHAR(73),
    created_at               TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    state_changed_at         TIMESTAMPTZ  NOT NULL,
    expires_at               TIMESTAMPTZ,
    CONSTRAINT pk_messaging_conversations PRIMARY KEY (id),
    CONSTRAINT uq_messaging_conversations_scope UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_messaging_conversations_group UNIQUE (mls_group_id),
    CONSTRAINT fk_messaging_conversations_policy FOREIGN KEY (environment_id, policy_snapshot_id)
        REFERENCES messaging_policy_snapshots (environment_id, id),
    CONSTRAINT ck_messaging_conversations_context CHECK (context_class IN
        ('DIRECT', 'GROUP', 'MEETING', 'TRANSACTION', 'CONSULTATION', 'SUPPORT', 'ENTERPRISE', 'DEVELOPER_DEFINED')),
    CONSTRAINT ck_messaging_conversations_context_ref CHECK (
        (context_namespace IS NULL AND context_ref IS NULL AND context_ref_version IS NULL)
        OR (context_namespace ~ '^[a-z][a-z0-9-]{0,62}$'
            AND length(context_ref) BETWEEN 1 AND 256 AND context_ref ~ '^[A-Za-z0-9._:/-]+$'
            AND context_ref_version BETWEEN 1 AND 2147483647)),
    CONSTRAINT ck_messaging_conversations_profile CHECK (
        (security_profile = 'PRIVATE_E2EE' AND protocol = 'MLS_1_0'
            AND ciphersuite = 'MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519'
            AND octet_length(mls_group_id) = 32)
        OR (security_profile = 'MANAGED' AND protocol = 'CFMEET_MANAGED_V1'
            AND ciphersuite IS NULL AND mls_group_id IS NULL)),
    CONSTRAINT ck_messaging_conversations_security_policy CHECK (security_policy_version >= 1),
    CONSTRAINT ck_messaging_conversations_state CHECK (lifecycle_state IN
        ('ACTIVE', 'LOCKED', 'CLOSED', 'ARCHIVED', 'EXPIRED', 'DELETED')),
    CONSTRAINT ck_messaging_conversations_reason CHECK (lifecycle_reason IS NULL
        OR lifecycle_reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_conversations_version CHECK (version >= 1 AND policy_version >= 0),
    CONSTRAINT ck_messaging_conversations_direct CHECK ((context_class = 'DIRECT') = (direct_pair_key IS NOT NULL)),
    CONSTRAINT ck_messaging_conversations_pair CHECK (direct_pair_key IS NULL
        OR direct_pair_key ~ '^[0-9a-f-]{36}:[0-9a-f-]{36}$'),
    CONSTRAINT ck_messaging_conversations_expiry CHECK (expires_at IS NULL OR expires_at > created_at)
);
-- One live direct conversation per principal pair, environment and security profile
-- (profiles are never merged). DELETED / EXPIRED ones do not block a new one.
CREATE UNIQUE INDEX uq_messaging_conversations_direct_pair ON messaging_conversations
    (environment_id, security_profile, direct_pair_key)
    WHERE context_class = 'DIRECT' AND lifecycle_state IN ('ACTIVE', 'LOCKED', 'CLOSED', 'ARCHIVED');
-- One live conversation per opaque context reference of a context class.
CREATE UNIQUE INDEX uq_messaging_conversations_context_ref ON messaging_conversations
    (environment_id, context_class, context_namespace, context_ref)
    WHERE context_ref IS NOT NULL AND lifecycle_state IN ('ACTIVE', 'LOCKED', 'CLOSED', 'ARCHIVED');
CREATE INDEX ix_messaging_conversations_environment ON messaging_conversations (environment_id, created_at DESC, id DESC);
CREATE INDEX ix_messaging_conversations_expiry ON messaging_conversations (expires_at)
    WHERE expires_at IS NOT NULL AND lifecycle_state IN ('ACTIVE', 'LOCKED', 'CLOSED');

-- Identity, context and security profile are immutable; lifecycle moves only along the
-- documented transitions; every change advances the version by exactly one.
CREATE FUNCTION messaging_conversations_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: conversations are never deleted physically' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.security_profile <> OLD.security_profile OR NEW.protocol <> OLD.protocol
       OR NEW.ciphersuite IS DISTINCT FROM OLD.ciphersuite OR NEW.mls_group_id IS DISTINCT FROM OLD.mls_group_id
       OR NEW.security_policy_version <> OLD.security_policy_version THEN
        RAISE EXCEPTION 'messaging: a conversation''s security profile is immutable' USING ERRCODE = 'P0002';
    END IF;
    IF NEW.id <> OLD.id OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.context_class <> OLD.context_class
       OR NEW.context_namespace IS DISTINCT FROM OLD.context_namespace
       OR NEW.context_ref IS DISTINCT FROM OLD.context_ref
       OR NEW.context_ref_version IS DISTINCT FROM OLD.context_ref_version
       OR NEW.created_by_principal_id <> OLD.created_by_principal_id
       OR NEW.created_by_credential_id <> OLD.created_by_credential_id
       OR NEW.direct_pair_key IS DISTINCT FROM OLD.direct_pair_key
       OR NEW.created_at <> OLD.created_at OR NEW.policy_snapshot_id <> OLD.policy_snapshot_id
       OR NEW.policy_version <> OLD.policy_version THEN
        RAISE EXCEPTION 'messaging: conversation identity and context are immutable' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.lifecycle_state <> OLD.lifecycle_state AND NOT (
           (OLD.lifecycle_state = 'ACTIVE'   AND NEW.lifecycle_state IN ('LOCKED', 'CLOSED', 'ARCHIVED', 'EXPIRED', 'DELETED'))
        OR (OLD.lifecycle_state = 'LOCKED'   AND NEW.lifecycle_state IN ('ACTIVE', 'CLOSED', 'ARCHIVED', 'EXPIRED', 'DELETED'))
        OR (OLD.lifecycle_state = 'CLOSED'   AND NEW.lifecycle_state IN ('ACTIVE', 'ARCHIVED', 'EXPIRED', 'DELETED'))
        OR (OLD.lifecycle_state = 'ARCHIVED' AND NEW.lifecycle_state IN ('ACTIVE', 'DELETED'))
        OR (OLD.lifecycle_state = 'EXPIRED'  AND NEW.lifecycle_state IN ('ARCHIVED', 'DELETED'))) THEN
        RAISE EXCEPTION 'messaging: conversation transition % -> % refused', OLD.lifecycle_state, NEW.lifecycle_state
            USING ERRCODE = 'P0003';
    END IF;
    IF OLD.lifecycle_state = 'DELETED' THEN
        RAISE EXCEPTION 'messaging: a deleted conversation is terminal' USING ERRCODE = 'P0003';
    END IF;
    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'messaging: conversation version must advance by one' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_conversations_guard BEFORE UPDATE OR DELETE ON messaging_conversations
    FOR EACH ROW EXECUTE FUNCTION messaging_conversations_guard();

CREATE TABLE messaging_conversation_members (
    conversation_id  UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    role             VARCHAR(16)  NOT NULL,
    joined_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_conversation_members PRIMARY KEY (conversation_id, principal_id),
    CONSTRAINT fk_messaging_conversation_members_conversation FOREIGN KEY (conversation_id)
        REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_conversation_members_role CHECK (role IN ('OWNER', 'MEMBER'))
);
CREATE INDEX ix_messaging_conversation_members_principal ON messaging_conversation_members (environment_id, principal_id);

CREATE TABLE messaging_conversation_sequences (
    conversation_id  UUID    NOT NULL,
    next_sequence    BIGINT  NOT NULL DEFAULT 1,
    CONSTRAINT pk_messaging_conversation_sequences PRIMARY KEY (conversation_id),
    CONSTRAINT fk_messaging_conversation_sequences_conversation FOREIGN KEY (conversation_id)
        REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_conversation_sequences_next CHECK (next_sequence >= 1)
);
CREATE FUNCTION messaging_sequences_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' OR NEW.next_sequence < OLD.next_sequence THEN
        RAISE EXCEPTION 'messaging: message sequences only advance' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_sequences_guard BEFORE UPDATE OR DELETE ON messaging_conversation_sequences
    FOR EACH ROW EXECUTE FUNCTION messaging_sequences_guard();

CREATE TABLE messaging_idempotency_keys (
    environment_id   UUID         NOT NULL,
    idempotency_key  VARCHAR(128) NOT NULL,
    operation        VARCHAR(48)  NOT NULL,
    request_digest   CHAR(64)     NOT NULL,
    response_status  SMALLINT,
    response_view    JSONB,
    created_at       TIMESTAMPTZ  NOT NULL,
    expires_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_idempotency_keys PRIMARY KEY (environment_id, operation, idempotency_key),
    CONSTRAINT ck_messaging_idempotency_keys_key CHECK (idempotency_key ~ '^[A-Za-z0-9._:-]{8,128}$'),
    CONSTRAINT ck_messaging_idempotency_keys_digest CHECK (request_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_idempotency_keys_response CHECK ((response_status IS NULL) = (response_view IS NULL)),
    CONSTRAINT ck_messaging_idempotency_keys_expiry CHECK (expires_at > created_at)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_idempotency_keys;
DROP TABLE IF EXISTS messaging_conversation_sequences;
DROP FUNCTION IF EXISTS messaging_sequences_guard();
DROP TABLE IF EXISTS messaging_conversation_members;
DROP TABLE IF EXISTS messaging_conversations;
DROP FUNCTION IF EXISTS messaging_conversations_guard();
DROP TABLE IF EXISTS messaging_policy_snapshots;
DROP TABLE IF EXISTS messaging_application_policies;
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
