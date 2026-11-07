//! Organizations Service foundation (Blueprint v1.0 Sections 3–5).
//!
//! * `organizations` — the organization aggregate (ids preserved on import). Slugs keep
//!   the legacy Meeting Service rule in the schema (`^[a-z0-9][a-z0-9-]*$`, ≤ 100) so
//!   imported rows are stored unchanged; the API applies the stricter contract pattern to
//!   every new or changed slug.
//! * `organization_profiles` — 1:1 descriptive profile, optional self-declared legal
//!   registration.
//! * `organization_memberships` (+ append-only `organization_membership_history`).
//! * `organization_invitations` — SHA-256 token digests only; at most one PENDING per
//!   organization and normalized email.
//! * `organization_audit_events` — append-only by trigger.
//! * `organization_outbox` — transactional outbox (written with every change).
//! * `organization_idempotency_records`, `organization_service_nonces`.
//! * `organization_import_runs` — Meeting Service migration import receipts.
//! * `organization_platform_scopes`, `organization_provisioning` — Section 7 (empty).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE FUNCTION organization_append_only() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '%: append-only', TG_TABLE_NAME USING ERRCODE = 'check_violation';
END $$;

CREATE TABLE organizations (
    id                  UUID         NOT NULL,
    name                VARCHAR(200) NOT NULL,
    slug                VARCHAR(100) NOT NULL,
    lifecycle_status    VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    lifecycle_reason    VARCHAR(32),
    provisioning_state  VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    creation_source     VARCHAR(16)  NOT NULL,
    created_by          UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    version             BIGINT       NOT NULL DEFAULT 1,
    archived_at         TIMESTAMPTZ,
    profile_policy      JSONB        NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT pk_organizations PRIMARY KEY (id),
    CONSTRAINT uq_organizations_slug UNIQUE (slug),
    CONSTRAINT ck_organizations_name CHECK (char_length(btrim(name)) BETWEEN 1 AND 200),
    CONSTRAINT ck_organizations_slug CHECK (slug ~ '^[a-z0-9][a-z0-9-]*$'),
    CONSTRAINT ck_organizations_lifecycle CHECK (lifecycle_status IN ('ACTIVE', 'SUSPENDED', 'ARCHIVED')),
    CONSTRAINT ck_organizations_reason CHECK (lifecycle_reason IS NULL OR lifecycle_reason IN ('OWNER_REQUEST', 'OPERATOR_ACTION', 'IMPORTED')),
    CONSTRAINT ck_organizations_provisioning CHECK (provisioning_state IN ('NOT_REQUIRED', 'PENDING', 'IN_PROGRESS', 'READY', 'FAILED')),
    CONSTRAINT ck_organizations_source CHECK (creation_source IN ('SELF_SERVICE', 'PLATFORM_ADMIN', 'IMPORTED')),
    CONSTRAINT ck_organizations_archived CHECK ((lifecycle_status = 'ARCHIVED') = (archived_at IS NOT NULL)),
    CONSTRAINT ck_organizations_version CHECK (version >= 1),
    CONSTRAINT ck_organizations_policy CHECK (jsonb_typeof(profile_policy) = 'object')
);
CREATE INDEX ix_organizations_self_service_creator ON organizations (created_by, created_at)
    WHERE creation_source = 'SELF_SERVICE';
CREATE INDEX ix_organizations_status ON organizations (lifecycle_status, id);

CREATE TABLE organization_profiles (
    organization_id             UUID          NOT NULL,
    description                 VARCHAR(2000),
    organization_type           VARCHAR(60),
    industry                    VARCHAR(60),
    country_code                CHAR(2),
    city                        VARCHAR(120),
    operating_address           VARCHAR(500),
    logo_ref                    VARCHAR(500),
    public_contact_email        VARCHAR(254),
    legal_registered_name       VARCHAR(200),
    legal_registration_number   VARCHAR(100),
    legal_incorporated_on       DATE,
    legal_verification          VARCHAR(16),
    updated_at                  TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_organization_profiles PRIMARY KEY (organization_id),
    CONSTRAINT fk_organization_profiles_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_profiles_country CHECK (country_code IS NULL OR country_code ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_organization_profiles_verification CHECK (legal_verification IS NULL OR legal_verification IN ('SELF_DECLARED', 'VERIFIED')),
    -- legalRegistration is either absent (every legal column NULL) or present with a
    -- verification state.
    CONSTRAINT ck_organization_profiles_legal CHECK (
        legal_verification IS NOT NULL
        OR (legal_registered_name IS NULL AND legal_registration_number IS NULL AND legal_incorporated_on IS NULL))
);

CREATE TABLE organization_memberships (
    id                  UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    user_id             UUID         NOT NULL,
    role                VARCHAR(16)  NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    joined_via          VARCHAR(24)  NOT NULL,
    created_by          UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    removed_at          TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_organization_memberships PRIMARY KEY (id),
    CONSTRAINT fk_organization_memberships_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_memberships_role CHECK (role IN ('OWNER', 'ADMIN', 'MANAGER', 'MEMBER', 'GUEST')),
    CONSTRAINT ck_organization_memberships_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'REMOVED')),
    CONSTRAINT ck_organization_memberships_joined CHECK (joined_via IN ('CREATOR', 'INVITATION', 'IMPORTED', 'OPERATOR', 'OWNERSHIP_TRANSFER')),
    CONSTRAINT ck_organization_memberships_removed CHECK (status <> 'REMOVED' OR removed_at IS NOT NULL),
    CONSTRAINT ck_organization_memberships_version CHECK (version >= 1)
);
-- One non-removed membership per organization + user.
CREATE UNIQUE INDEX uq_organization_memberships_current
    ON organization_memberships (organization_id, user_id) WHERE status <> 'REMOVED';
CREATE INDEX ix_organization_memberships_user ON organization_memberships (user_id) WHERE status <> 'REMOVED';
CREATE INDEX ix_organization_memberships_owners ON organization_memberships (organization_id, role, status);

CREATE TABLE organization_membership_history (
    seq                 BIGSERIAL    NOT NULL,
    id                  UUID         NOT NULL,
    membership_id       UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    user_id             UUID         NOT NULL,
    change_kind         VARCHAR(32)  NOT NULL,
    from_role           VARCHAR(16),
    to_role             VARCHAR(16)  NOT NULL,
    from_status         VARCHAR(16),
    to_status           VARCHAR(16)  NOT NULL,
    membership_version  BIGINT       NOT NULL,
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_user_id       UUID,
    actor_service       VARCHAR(64)  NOT NULL,
    request_id          VARCHAR(64),
    occurred_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_membership_history PRIMARY KEY (seq),
    CONSTRAINT uq_organization_membership_history_id UNIQUE (id),
    CONSTRAINT ck_organization_membership_history_kind CHECK (change_kind IN ('CREATED', 'IMPORTED', 'ROLE_CHANGED', 'STATUS_CHANGED', 'ROLE_AND_STATUS_CHANGED', 'LEFT', 'OWNERSHIP_TRANSFER')),
    CONSTRAINT ck_organization_membership_history_actor CHECK (actor_kind IN ('USER', 'OPERATOR', 'SYSTEM'))
);
CREATE INDEX ix_organization_membership_history_membership ON organization_membership_history (membership_id, seq);
CREATE TRIGGER tr_organization_membership_history_append_only BEFORE UPDATE OR DELETE ON organization_membership_history
    FOR EACH ROW EXECUTE FUNCTION organization_append_only();
CREATE TRIGGER tr_organization_membership_history_no_truncate BEFORE TRUNCATE ON organization_membership_history
    FOR EACH STATEMENT EXECUTE FUNCTION organization_append_only();

CREATE TABLE organization_invitations (
    id                  UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    token_hash          CHAR(64)     NOT NULL,
    email_normalized    VARCHAR(254) NOT NULL,
    email_hint          VARCHAR(254) NOT NULL,
    role                VARCHAR(16)  NOT NULL,
    status              VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    invited_by          UUID         NOT NULL,
    invitee_user_id     UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    expires_at          TIMESTAMPTZ  NOT NULL DEFAULT (now() + interval '7 days'),
    decided_at          TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_organization_invitations PRIMARY KEY (id),
    CONSTRAINT uq_organization_invitations_token UNIQUE (token_hash),
    CONSTRAINT fk_organization_invitations_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_invitations_token CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_organization_invitations_role CHECK (role IN ('ADMIN', 'MANAGER', 'MEMBER', 'GUEST')),
    CONSTRAINT ck_organization_invitations_status CHECK (status IN ('PENDING', 'ACCEPTED', 'DECLINED', 'REVOKED', 'EXPIRED')),
    CONSTRAINT ck_organization_invitations_decided CHECK ((status = 'PENDING') = (decided_at IS NULL)),
    CONSTRAINT ck_organization_invitations_expiry CHECK (expires_at > created_at),
    CONSTRAINT ck_organization_invitations_version CHECK (version >= 1)
);
CREATE UNIQUE INDEX uq_organization_invitations_pending
    ON organization_invitations (organization_id, email_normalized) WHERE status = 'PENDING';
CREATE INDEX ix_organization_invitations_due ON organization_invitations (expires_at) WHERE status = 'PENDING';
CREATE INDEX ix_organization_invitations_org ON organization_invitations (organization_id, created_at DESC);

CREATE TABLE organization_audit_events (
    seq                 BIGSERIAL    NOT NULL,
    id                  UUID         NOT NULL,
    organization_id     UUID,
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_user_id       UUID,
    actor_service       VARCHAR(64)  NOT NULL,
    action              VARCHAR(64)  NOT NULL,
    target_type         VARCHAR(32)  NOT NULL,
    target_id           UUID,
    summary             JSONB        NOT NULL DEFAULT '{}'::jsonb,
    request_id          VARCHAR(64),
    correlation_id      VARCHAR(64),
    occurred_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_audit_events PRIMARY KEY (seq),
    CONSTRAINT uq_organization_audit_events_id UNIQUE (id),
    CONSTRAINT ck_organization_audit_events_actor CHECK (actor_kind IN ('USER', 'OPERATOR', 'SYSTEM')),
    CONSTRAINT ck_organization_audit_events_action CHECK (action ~ '^[a-z][a-z_]*\.[a-z][a-z_.]*$'),
    CONSTRAINT ck_organization_audit_events_summary CHECK (
        jsonb_typeof(summary) = 'object' AND octet_length(summary::text) <= 4096
        AND NOT (summary ?| ARRAY['email', 'token', 'tokenHash', 'password', 'secret', 'phoneNumber']))
);
CREATE INDEX ix_organization_audit_events_org ON organization_audit_events (organization_id, seq DESC);
CREATE TRIGGER tr_organization_audit_events_append_only BEFORE UPDATE OR DELETE ON organization_audit_events
    FOR EACH ROW EXECUTE FUNCTION organization_append_only();
CREATE TRIGGER tr_organization_audit_events_no_truncate BEFORE TRUNCATE ON organization_audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION organization_append_only();

CREATE TABLE organization_outbox (
    seq                 BIGSERIAL    NOT NULL,
    event_id            UUID         NOT NULL,
    destination         VARCHAR(64)  NOT NULL,
    organization_id     UUID         NOT NULL,
    aggregate_type      VARCHAR(16)  NOT NULL,
    aggregate_id        UUID         NOT NULL,
    aggregate_version   BIGINT       NOT NULL,
    event_type          VARCHAR(64)  NOT NULL,
    payload             JSONB        NOT NULL,
    state               VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    attempts            INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    lease_id            UUID,
    lease_until         TIMESTAMPTZ,
    delivered_at        TIMESTAMPTZ,
    last_error_class    VARCHAR(64),
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_organization_outbox PRIMARY KEY (seq),
    CONSTRAINT uq_organization_outbox_event UNIQUE (event_id, destination),
    CONSTRAINT ck_organization_outbox_destination CHECK (destination ~ '^cfmeet\.[a-z][a-z0-9-]*$'),
    CONSTRAINT ck_organization_outbox_aggregate CHECK (aggregate_type IN ('organization', 'membership', 'invitation', 'team', 'policy', 'scope', 'provisioning')),
    CONSTRAINT ck_organization_outbox_state CHECK (state IN ('PENDING', 'INFLIGHT', 'DELIVERED', 'DEAD')),
    CONSTRAINT ck_organization_outbox_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_organization_outbox_lease CHECK ((state = 'INFLIGHT') = (lease_id IS NOT NULL AND lease_until IS NOT NULL)),
    CONSTRAINT ck_organization_outbox_payload CHECK (
        jsonb_typeof(payload) = 'object'
        AND NOT (payload ?| ARRAY['email', 'token', 'tokenHash', 'password', 'secret', 'phoneNumber']))
);
CREATE INDEX ix_organization_outbox_due ON organization_outbox (next_attempt_at, seq) WHERE state IN ('PENDING', 'INFLIGHT');
CREATE INDEX ix_organization_outbox_aggregate ON organization_outbox (aggregate_id, aggregate_version);

CREATE TABLE organization_idempotency_records (
    scope_key           VARCHAR(200) NOT NULL,
    idempotency_key     VARCHAR(128) NOT NULL,
    request_digest      CHAR(64)     NOT NULL,
    response_status     SMALLINT     NOT NULL,
    response_body       JSONB        NOT NULL,
    resource_id         UUID,
    created_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_idempotency_records PRIMARY KEY (scope_key, idempotency_key),
    CONSTRAINT ck_organization_idempotency_records_key CHECK (idempotency_key ~ '^[\x21-\x7e]{1,128}$'),
    CONSTRAINT ck_organization_idempotency_records_body CHECK (NOT (response_body ? 'token'))
);
CREATE INDEX ix_organization_idempotency_records_created ON organization_idempotency_records (created_at);

CREATE TABLE organization_service_nonces (
    issuer              VARCHAR(64)  NOT NULL,
    token_id            VARCHAR(80)  NOT NULL,
    expires_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_service_nonces PRIMARY KEY (issuer, token_id)
);
CREATE INDEX ix_organization_service_nonces_expiry ON organization_service_nonces (expires_at);

CREATE TABLE organization_import_runs (
    id                  UUID         NOT NULL,
    source              VARCHAR(64)  NOT NULL,
    exported_at         TIMESTAMPTZ  NOT NULL,
    checksum            CHAR(64)     NOT NULL,
    organizations_count INTEGER      NOT NULL,
    memberships_count   INTEGER      NOT NULL,
    imported            INTEGER      NOT NULL,
    unchanged           INTEGER      NOT NULL,
    conflicts           JSONB        NOT NULL DEFAULT '[]'::jsonb,
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_service       VARCHAR(64)  NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_import_runs PRIMARY KEY (id),
    CONSTRAINT ck_organization_import_runs_checksum CHECK (checksum ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_organization_import_runs_conflicts CHECK (jsonb_typeof(conflicts) = 'array')
);
CREATE INDEX ix_organization_import_runs_created ON organization_import_runs (created_at DESC);

-- Section 7: organization <-> Platform Access technical scope mapping (ADR 0056 §4).
-- Ids and verification evidence only; Platform Access stays the tenant authority.
CREATE TABLE organization_platform_scopes (
    id                  UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    capability          VARCHAR(64)  NOT NULL,
    tenant_id           UUID,
    application_id      UUID,
    environment_id      UUID,
    mapping_state       VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    mapping_version     BIGINT       NOT NULL DEFAULT 1,
    verified_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_platform_scopes PRIMARY KEY (id),
    CONSTRAINT fk_organization_platform_scopes_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_platform_scopes_state CHECK (mapping_state IN ('PENDING', 'VERIFYING', 'READY', 'FAILED', 'SUSPENDED', 'RETIRED')),
    CONSTRAINT ck_organization_platform_scopes_ready CHECK (
        mapping_state <> 'READY'
        OR (tenant_id IS NOT NULL AND application_id IS NOT NULL AND environment_id IS NOT NULL AND verified_at IS NOT NULL)),
    CONSTRAINT ck_organization_platform_scopes_version CHECK (mapping_version >= 1)
);
CREATE UNIQUE INDEX uq_organization_platform_scopes_active
    ON organization_platform_scopes (organization_id, capability) WHERE mapping_state <> 'RETIRED';

-- Section 7: provisioning workflow (saga) state per organization.
CREATE TABLE organization_provisioning (
    workflow_id         UUID         NOT NULL,
    organization_id     UUID         NOT NULL,
    idempotency_key     VARCHAR(128) NOT NULL,
    state               VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    step                VARCHAR(64),
    attempts            INTEGER      NOT NULL DEFAULT 0,
    last_error_class    VARCHAR(64),
    next_attempt_at     TIMESTAMPTZ,
    lease_id            UUID,
    lease_until         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_organization_provisioning PRIMARY KEY (workflow_id),
    CONSTRAINT uq_organization_provisioning_key UNIQUE (idempotency_key),
    CONSTRAINT fk_organization_provisioning_organization FOREIGN KEY (organization_id) REFERENCES organizations (id) ON DELETE RESTRICT,
    CONSTRAINT ck_organization_provisioning_state CHECK (state IN ('PENDING', 'IN_PROGRESS', 'READY', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_organization_provisioning_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_organization_provisioning_error CHECK (last_error_class IS NULL OR last_error_class ~ '^[A-Z][A-Z0-9_]{0,63}$')
);
CREATE UNIQUE INDEX uq_organization_provisioning_open
    ON organization_provisioning (organization_id) WHERE state IN ('PENDING', 'IN_PROGRESS');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS organization_provisioning;
DROP TABLE IF EXISTS organization_platform_scopes;
DROP TABLE IF EXISTS organization_import_runs;
DROP TABLE IF EXISTS organization_service_nonces;
DROP TABLE IF EXISTS organization_idempotency_records;
DROP TABLE IF EXISTS organization_outbox;
DROP TABLE IF EXISTS organization_audit_events;
DROP TABLE IF EXISTS organization_invitations;
DROP TABLE IF EXISTS organization_membership_history;
DROP TABLE IF EXISTS organization_memberships;
DROP TABLE IF EXISTS organization_profiles;
DROP TABLE IF EXISTS organizations;
DROP FUNCTION IF EXISTS organization_append_only();
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
