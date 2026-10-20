//! Reports, moderation, exports and data deletion (Part V Part G, §84–§94).
//!
//! * `messaging_reports` — user reports. PRIVATE_E2EE evidence exists only when the
//!   REPORTER voluntarily disclosed what their own device decrypted (sealed at rest under
//!   the managed key ring; marked unverifiable by the server); MANAGED reports may
//!   reference server-visible messages. The reported principal is never told who
//!   reported.
//! * `messaging_moderation_actions` — append-only record of every moderation action
//!   (who, what, why, linked report). A trigger refuses updates and deletes.
//! * `messaging_exports` — authorized MANAGED-only exports (never E2EE plaintext): a
//!   leased job writing one object to protected storage, downloadable through short-lived
//!   grants until it expires.
//! * `messaging_deletion_requests` — principal erasure / conversation purge requests,
//!   executed by a leased worker that respects legal holds (held items are counted and
//!   preserved, the request ends PARTIALLY_HELD).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_reports (
    id                       UUID          NOT NULL,
    tenant_id                UUID          NOT NULL,
    application_id           UUID          NOT NULL,
    environment_id           UUID          NOT NULL,
    conversation_id          UUID          NOT NULL,
    security_profile         VARCHAR(16)   NOT NULL,
    reporter_principal_id    UUID          NOT NULL,
    reported_principal_id    UUID,
    message_ids              UUID[]        NOT NULL DEFAULT '{}',
    category                 VARCHAR(24)   NOT NULL,
    evidence_kind            VARCHAR(24)   NOT NULL,
    sealed_evidence          BYTEA,
    evidence_nonce           BYTEA,
    evidence_key_id          VARCHAR(32),
    status                   VARCHAR(16)   NOT NULL DEFAULT 'OPEN',
    resolution               VARCHAR(48),
    resolved_at              TIMESTAMPTZ,
    resolved_by_credential_id UUID,
    created_at               TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_reports PRIMARY KEY (id),
    CONSTRAINT fk_messaging_reports_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_reports_category CHECK (category IN
        ('SPAM', 'HARASSMENT', 'HATE', 'VIOLENCE', 'SEXUAL_CONTENT', 'CHILD_SAFETY', 'SELF_HARM', 'ILLEGAL_CONTENT', 'IMPERSONATION', 'OTHER')),
    CONSTRAINT ck_messaging_reports_messages CHECK (cardinality(message_ids) <= 20),
    CONSTRAINT ck_messaging_reports_evidence CHECK (
        evidence_kind = 'NONE'
        OR (evidence_kind = 'MANAGED_REFERENCE' AND security_profile = 'MANAGED')
        OR (evidence_kind = 'REPORTER_DISCLOSURE' AND security_profile = 'PRIVATE_E2EE' AND sealed_evidence IS NOT NULL)),
    CONSTRAINT ck_messaging_reports_sealed CHECK ((sealed_evidence IS NULL) = (evidence_nonce IS NULL)
        AND (sealed_evidence IS NULL) = (evidence_key_id IS NULL)),
    CONSTRAINT ck_messaging_reports_status CHECK (status IN ('OPEN', 'ACTIONED', 'DISMISSED')),
    CONSTRAINT ck_messaging_reports_resolved CHECK ((status = 'OPEN') = (resolved_at IS NULL)),
    CONSTRAINT ck_messaging_reports_resolution CHECK (resolution IS NULL OR resolution ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE INDEX ix_messaging_reports_queue ON messaging_reports (environment_id, status, created_at);
CREATE INDEX ix_messaging_reports_reporter ON messaging_reports (environment_id, reporter_principal_id, created_at);

CREATE TABLE messaging_moderation_actions (
    id                    UUID          NOT NULL,
    tenant_id             UUID          NOT NULL,
    application_id        UUID          NOT NULL,
    environment_id        UUID          NOT NULL,
    kind                  VARCHAR(24)   NOT NULL,
    conversation_id       UUID,
    message_id            UUID,
    principal_id          UUID,
    report_id             UUID,
    reason                VARCHAR(48)   NOT NULL,
    effective_until       TIMESTAMPTZ,
    credential_id         UUID          NOT NULL,
    created_at            TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_moderation_actions PRIMARY KEY (id),
    CONSTRAINT fk_messaging_moderation_actions_report FOREIGN KEY (report_id) REFERENCES messaging_reports (id),
    CONSTRAINT ck_messaging_moderation_actions_kind CHECK (kind IN
        ('REMOVE_MESSAGE', 'MUTE_MEMBER', 'UNMUTE_MEMBER', 'REMOVE_MEMBER', 'RESTRICT_PRINCIPAL', 'LIFT_RESTRICTION', 'DISMISS_REPORT')),
    CONSTRAINT ck_messaging_moderation_actions_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_moderation_actions_target CHECK (
        (kind = 'REMOVE_MESSAGE' AND conversation_id IS NOT NULL AND message_id IS NOT NULL)
        OR (kind IN ('MUTE_MEMBER', 'UNMUTE_MEMBER', 'REMOVE_MEMBER') AND conversation_id IS NOT NULL AND principal_id IS NOT NULL)
        OR (kind IN ('RESTRICT_PRINCIPAL', 'LIFT_RESTRICTION') AND principal_id IS NOT NULL)
        OR (kind = 'DISMISS_REPORT' AND report_id IS NOT NULL))
);
CREATE INDEX ix_messaging_moderation_actions_env ON messaging_moderation_actions (environment_id, created_at);
CREATE FUNCTION messaging_moderation_actions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'messaging: moderation actions are append-only' USING ERRCODE = 'P0001';
END $$;
CREATE TRIGGER trg_messaging_moderation_actions_guard BEFORE UPDATE OR DELETE ON messaging_moderation_actions
    FOR EACH ROW EXECUTE FUNCTION messaging_moderation_actions_guard();

CREATE TABLE messaging_exports (
    id                        UUID          NOT NULL,
    tenant_id                 UUID          NOT NULL,
    application_id            UUID          NOT NULL,
    environment_id            UUID          NOT NULL,
    conversation_id           UUID          NOT NULL,
    requested_by_credential_id UUID         NOT NULL,
    status                    VARCHAR(16)   NOT NULL DEFAULT 'PENDING',
    object_key                VARCHAR(512)  NOT NULL,
    size                      BIGINT,
    sha256                    CHAR(64),
    message_count             BIGINT,
    failure                   VARCHAR(48),
    attempts                  INTEGER       NOT NULL DEFAULT 0,
    lease_token               UUID,
    lease_expires_at          TIMESTAMPTZ,
    created_at                TIMESTAMPTZ   NOT NULL,
    completed_at              TIMESTAMPTZ,
    expires_at                TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_exports PRIMARY KEY (id),
    CONSTRAINT fk_messaging_exports_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT uq_messaging_exports_object UNIQUE (object_key),
    CONSTRAINT ck_messaging_exports_status CHECK (status IN ('PENDING', 'READY', 'FAILED', 'EXPIRED')),
    CONSTRAINT ck_messaging_exports_key CHECK (object_key ~ '^exp/[0-9a-f]{32}/[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_exports_ready CHECK ((status = 'READY') = (sha256 IS NOT NULL AND size IS NOT NULL)
        OR status = 'EXPIRED'),
    CONSTRAINT ck_messaging_exports_failure CHECK (failure IS NULL OR failure ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_exports_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL))
);
CREATE INDEX ix_messaging_exports_due ON messaging_exports (created_at) WHERE status = 'PENDING';
CREATE INDEX ix_messaging_exports_expiry ON messaging_exports (expires_at) WHERE status = 'READY';

CREATE TABLE messaging_deletion_requests (
    id                        UUID          NOT NULL,
    tenant_id                 UUID          NOT NULL,
    application_id            UUID          NOT NULL,
    environment_id            UUID          NOT NULL,
    kind                      VARCHAR(24)   NOT NULL,
    principal_id              UUID,
    conversation_id           UUID,
    requested_by_credential_id UUID         NOT NULL,
    status                    VARCHAR(16)   NOT NULL DEFAULT 'PENDING',
    messages_purged           BIGINT        NOT NULL DEFAULT 0,
    attachments_deleted       BIGINT        NOT NULL DEFAULT 0,
    held_items                BIGINT        NOT NULL DEFAULT 0,
    lease_token               UUID,
    lease_expires_at          TIMESTAMPTZ,
    created_at                TIMESTAMPTZ   NOT NULL,
    completed_at              TIMESTAMPTZ,
    CONSTRAINT pk_messaging_deletion_requests PRIMARY KEY (id),
    CONSTRAINT ck_messaging_deletion_requests_kind CHECK (
        (kind = 'PRINCIPAL_ERASURE' AND principal_id IS NOT NULL AND conversation_id IS NULL)
        OR (kind = 'CONVERSATION_PURGE' AND conversation_id IS NOT NULL AND principal_id IS NULL)),
    CONSTRAINT ck_messaging_deletion_requests_status CHECK (status IN ('PENDING', 'COMPLETED', 'PARTIALLY_HELD')),
    CONSTRAINT ck_messaging_deletion_requests_done CHECK ((status = 'PENDING') = (completed_at IS NULL)),
    CONSTRAINT ck_messaging_deletion_requests_counts CHECK (messages_purged >= 0 AND attachments_deleted >= 0 AND held_items >= 0),
    CONSTRAINT ck_messaging_deletion_requests_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL))
);
CREATE INDEX ix_messaging_deletion_requests_due ON messaging_deletion_requests (created_at) WHERE status = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_deletion_requests;
DROP TABLE IF EXISTS messaging_exports;
DROP TABLE IF EXISTS messaging_moderation_actions;
DROP FUNCTION IF EXISTS messaging_moderation_actions_guard();
DROP TABLE IF EXISTS messaging_reports;
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
