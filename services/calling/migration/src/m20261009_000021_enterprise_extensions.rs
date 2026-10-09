//! Extensions and the per-Call enterprise context (Part F §54, §26–§36).
//!
//! * `enterprise_extensions` — internal, organization-scoped routable short codes (never
//!   public telephone numbers). One ACTIVE extension per number within a tenant /
//!   application / environment (partial unique index); disabling keeps the row so
//!   historical Calls keep their reference (offboarding never deletes Call evidence).
//! * Destinations are typed `(destination_type, destination_id)` pairs. The database
//!   proves every destination exists in the SAME tenant / application / environment
//!   (`enterprise_destination_exists`, extended by later migrations as destination
//!   kinds are added): a cross-tenant or dangling destination cannot be written.
//! * `enterprise_call_contexts` — one row per canonical Call that entered enterprise
//!   routing (the Call itself stays in `calls`; this is NOT a second Call model): the
//!   entry extension and its number / label SNAPSHOT (history survives renumbering or
//!   offboarding), the policy version relied on, the current destination, the hop budget
//!   and the visited set that make routing loops impossible, and the business outcome.
//! * `enterprise_routing_steps` — append-only routing evidence of each decision.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE enterprise_extensions (
    id               UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    extension_number VARCHAR(32)  NOT NULL,
    display_label    VARCHAR(100) NOT NULL,
    destination_type TEXT         NOT NULL,
    destination_id   UUID         NOT NULL,
    policy_id        UUID,
    status           TEXT         NOT NULL,
    version          BIGINT       NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    disabled_at      TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_extensions PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_extensions_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_extensions_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_extensions_policy
        FOREIGN KEY (tenant_id, application_id, environment_id, policy_id)
        REFERENCES enterprise_call_policies (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    -- Internal short codes only: 2-6 digits, or a lower-case word; never '+' or 7+ digits.
    CONSTRAINT ck_enterprise_extensions_number
        CHECK (extension_number ~ '^([0-9]{2,6}|[a-z][a-z0-9-]{1,31})$'),
    CONSTRAINT ck_enterprise_extensions_label CHECK (length(btrim(display_label)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_extensions_destination_type CHECK (destination_type IN (
        'ENTERPRISE_MEMBER', 'PRINCIPAL', 'RING_GROUP', 'QUEUE', 'IVR', 'VOICEMAIL', 'SCHEDULE')),
    CONSTRAINT ck_enterprise_extensions_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_extensions_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_extensions_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_extensions_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_extensions_active_number
    ON enterprise_extensions (tenant_id, application_id, environment_id, extension_number)
    WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_extensions_scope
    ON enterprise_extensions (tenant_id, application_id, environment_id, created_at, id);
CREATE INDEX idx_enterprise_extensions_destination
    ON enterprise_extensions (destination_type, destination_id) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_extensions_policy ON enterprise_extensions (policy_id)
    WHERE policy_id IS NOT NULL;
CREATE TRIGGER trg_enterprise_extensions_immutable
    BEFORE UPDATE ON enterprise_extensions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_extensions_version
    BEFORE UPDATE ON enterprise_extensions
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

-- Whether destination (kind, id) exists in the given scope. Later migrations replace this
-- function as destination kinds gain tables; an unknown kind never exists.
CREATE FUNCTION enterprise_destination_exists(
    t UUID, a UUID, e UUID, kind TEXT, target UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'PRINCIPAL' THEN EXISTS (SELECT 1 FROM platform_principals p
            WHERE p.tenant_id = t AND p.application_id = a AND p.environment_id = e AND p.id = target)
        WHEN 'ENTERPRISE_MEMBER' THEN EXISTS (SELECT 1 FROM platform_principals p
            WHERE p.tenant_id = t AND p.application_id = a AND p.environment_id = e AND p.id = target
              AND p.kind = 'ENTERPRISE_MEMBER')
        WHEN 'EXTENSION' THEN EXISTS (SELECT 1 FROM enterprise_extensions x
            WHERE x.tenant_id = t AND x.application_id = a AND x.environment_id = e AND x.id = target)
        ELSE FALSE
    END
$$;

-- Trigger: TG_ARGV[0] = type column, TG_ARGV[1] = id column (both may be NULL together).
CREATE FUNCTION enterprise_guard_destination() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    doc  JSONB := to_jsonb(NEW);
    kind TEXT  := doc ->> TG_ARGV[0];
    id   UUID  := (doc ->> TG_ARGV[1])::UUID;
BEGIN
    IF kind IS NULL AND id IS NULL THEN
        RETURN NEW;
    END IF;
    IF kind IS NULL OR id IS NULL OR NOT enterprise_destination_exists(
            NEW.tenant_id, NEW.application_id, NEW.environment_id, kind, id) THEN
        RAISE EXCEPTION 'enterprise invariant violated: %.% does not exist in the same scope',
            TG_TABLE_NAME, TG_ARGV[0]
            USING ERRCODE = 'foreign_key_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_enterprise_extensions_destination
    BEFORE INSERT OR UPDATE ON enterprise_extensions
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('destination_type', 'destination_id');

CREATE TABLE enterprise_call_contexts (
    call_id                  UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    entry_extension_id       UUID         NOT NULL,
    entry_extension_number   VARCHAR(32)  NOT NULL,
    entry_label              VARCHAR(100) NOT NULL,
    policy_id                UUID,
    policy_version           BIGINT       NOT NULL,
    phase                    TEXT         NOT NULL,
    current_destination_type TEXT,
    current_destination_id   UUID,
    hop_count                SMALLINT     NOT NULL,
    max_hops                 SMALLINT     NOT NULL,
    visited                  UUID[]       NOT NULL,
    answered_principal_id    UUID,
    outcome                  TEXT,
    version                  BIGINT       NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_enterprise_call_contexts PRIMARY KEY (call_id),
    CONSTRAINT uq_enterprise_call_contexts_owner_id
        UNIQUE (tenant_id, application_id, environment_id, call_id),
    CONSTRAINT fk_enterprise_call_contexts_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_call_contexts_extension
        FOREIGN KEY (tenant_id, application_id, environment_id, entry_extension_id)
        REFERENCES enterprise_extensions (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_call_contexts_answered
        FOREIGN KEY (tenant_id, application_id, environment_id, answered_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_call_contexts_number
        CHECK (entry_extension_number ~ '^([0-9]{2,6}|[a-z][a-z0-9-]{1,31})$'),
    CONSTRAINT ck_enterprise_call_contexts_policy_version CHECK (policy_version >= 0),
    CONSTRAINT ck_enterprise_call_contexts_phase CHECK (phase IN (
        'ROUTING', 'RINGING', 'RING_GROUP', 'QUEUED', 'IVR', 'VOICEMAIL', 'FORWARDED',
        'ANSWERED', 'ENDED')),
    CONSTRAINT ck_enterprise_call_contexts_destination_type CHECK (current_destination_type IS NULL
        OR current_destination_type IN ('ENTERPRISE_MEMBER', 'PRINCIPAL', 'EXTENSION', 'RING_GROUP',
            'QUEUE', 'IVR', 'VOICEMAIL', 'SCHEDULE', 'PUBLIC_CONTACT')),
    CONSTRAINT ck_enterprise_call_contexts_hops
        CHECK (hop_count >= 0 AND max_hops BETWEEN 1 AND 16 AND hop_count <= max_hops),
    CONSTRAINT ck_enterprise_call_contexts_visited CHECK (cardinality(visited) <= 32),
    CONSTRAINT ck_enterprise_call_contexts_outcome CHECK (outcome IS NULL OR outcome IN (
        'ANSWERED', 'VOICEMAIL', 'NO_ANSWER', 'DECLINED', 'ABANDONED', 'FAILED', 'FORWARDED_PUBLIC')),
    CONSTRAINT ck_enterprise_call_contexts_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_call_contexts_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_enterprise_call_contexts_extension
    ON enterprise_call_contexts (entry_extension_id, created_at DESC);
CREATE INDEX idx_enterprise_call_contexts_phase
    ON enterprise_call_contexts (phase) WHERE phase NOT IN ('ANSWERED', 'ENDED');
CREATE TRIGGER trg_enterprise_call_contexts_immutable
    BEFORE UPDATE ON enterprise_call_contexts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'call_id', 'tenant_id', 'application_id', 'environment_id', 'entry_extension_id',
        'entry_extension_number', 'entry_label', 'policy_id', 'policy_version', 'max_hops',
        'created_at');
CREATE TRIGGER trg_enterprise_call_contexts_version
    BEFORE UPDATE ON enterprise_call_contexts
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

CREATE TABLE enterprise_routing_steps (
    call_id          UUID        NOT NULL,
    step_number      INTEGER     NOT NULL,
    tenant_id        UUID        NOT NULL,
    application_id   UUID        NOT NULL,
    environment_id   UUID        NOT NULL,
    decision         TEXT        NOT NULL,
    destination_type TEXT,
    destination_id   UUID,
    reason           TEXT        NOT NULL,
    answer_group_id  UUID,
    decided_at       TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_enterprise_routing_steps PRIMARY KEY (call_id, step_number),
    CONSTRAINT fk_enterprise_routing_steps_context
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES enterprise_call_contexts (tenant_id, application_id, environment_id, call_id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_routing_steps_number CHECK (step_number >= 1),
    CONSTRAINT ck_enterprise_routing_steps_decision CHECK (decision ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_enterprise_routing_steps_reason CHECK (reason ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_enterprise_routing_steps_destination CHECK (destination_type IS NULL OR
        destination_type IN ('ENTERPRISE_MEMBER', 'PRINCIPAL', 'EXTENSION', 'RING_GROUP', 'QUEUE',
            'IVR', 'VOICEMAIL', 'SCHEDULE', 'PUBLIC_CONTACT'))
);
CREATE TRIGGER trg_enterprise_routing_steps_append_only
    BEFORE UPDATE OR DELETE ON enterprise_routing_steps
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS enterprise_routing_steps;
DROP TABLE IF EXISTS enterprise_call_contexts;
DROP TABLE IF EXISTS enterprise_extensions;
DROP FUNCTION IF EXISTS enterprise_guard_destination();
DROP FUNCTION IF EXISTS enterprise_destination_exists(UUID, UUID, UUID, TEXT, UUID);
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
