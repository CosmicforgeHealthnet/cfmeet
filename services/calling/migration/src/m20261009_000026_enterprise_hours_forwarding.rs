//! Business hours and forwarding (Part F §58, §77–§88).
//!
//! * `enterprise_business_hours` — a schedule in an IANA time zone (evaluated by a proven
//!   time-zone library in the service, never by hand-written DST arithmetic) with an
//!   OPEN route and a CLOSED route (typed, guarded destinations of the same scope).
//! * `enterprise_business_hours_windows` — weekly open windows (ISO day of week, local
//!   minutes; a window whose end is before its start crosses midnight into the next day)
//!   and the custom hours of an exception date (never crossing midnight).
//! * `enterprise_business_hours_exceptions` — a local date that is CLOSED or has
//!   CUSTOM_HOURS; it replaces that date's weekly hours entirely.
//! * `enterprise_public_contacts` — an admin-registered PROTECTED public destination: an
//!   opaque Privacy Relay ContactReference plus the Privacy Relay Session whose authority
//!   covers it. Never a telephone number. The `PUBLIC_CONTACT` destination type names one.
//! * `enterprise_forwarding_rules` — a principal's typed call forwarding (ALWAYS / BUSY /
//!   NO_ANSWER / UNAVAILABLE) to a typed destination. At most one ACTIVE rule per
//!   principal and condition. Loops are stopped at routing time (visited set + hop budget).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE enterprise_business_hours (
    id                      UUID         NOT NULL,
    tenant_id               UUID         NOT NULL,
    application_id          UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    name                    VARCHAR(100) NOT NULL,
    timezone                VARCHAR(64)  NOT NULL,
    open_destination_type   TEXT         NOT NULL,
    open_destination_id     UUID         NOT NULL,
    closed_destination_type TEXT         NOT NULL,
    closed_destination_id   UUID         NOT NULL,
    status                  TEXT         NOT NULL,
    version                 BIGINT       NOT NULL,
    created_at              TIMESTAMPTZ  NOT NULL,
    updated_at              TIMESTAMPTZ  NOT NULL,
    disabled_at             TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_business_hours PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_business_hours_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_business_hours_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_business_hours_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_business_hours_timezone
        CHECK (timezone ~ '^[A-Za-z][A-Za-z0-9_+-]*(/[A-Za-z0-9_+-]+){0,2}$'),
    CONSTRAINT ck_enterprise_business_hours_open_type CHECK (open_destination_type IN (
        'ENTERPRISE_MEMBER', 'PRINCIPAL', 'EXTENSION', 'RING_GROUP', 'QUEUE', 'IVR')),
    CONSTRAINT ck_enterprise_business_hours_closed_type CHECK (closed_destination_type IN (
        'ENTERPRISE_MEMBER', 'PRINCIPAL', 'EXTENSION', 'RING_GROUP', 'QUEUE', 'IVR', 'VOICEMAIL',
        'PUBLIC_CONTACT')),
    CONSTRAINT ck_enterprise_business_hours_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_business_hours_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_business_hours_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_business_hours_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_business_hours_active_name
    ON enterprise_business_hours (environment_id, lower(name)) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_business_hours_scope
    ON enterprise_business_hours (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_business_hours_immutable
    BEFORE UPDATE ON enterprise_business_hours
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_business_hours_version
    BEFORE UPDATE ON enterprise_business_hours
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_enterprise_business_hours_open
    BEFORE INSERT OR UPDATE ON enterprise_business_hours
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('open_destination_type', 'open_destination_id');
CREATE TRIGGER trg_enterprise_business_hours_closed
    BEFORE INSERT OR UPDATE ON enterprise_business_hours
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('closed_destination_type', 'closed_destination_id');

CREATE TABLE enterprise_business_hours_exceptions (
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    rule_id        UUID        NOT NULL,
    exception_date DATE        NOT NULL,
    kind           TEXT        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_enterprise_business_hours_exceptions PRIMARY KEY (rule_id, exception_date),
    CONSTRAINT fk_enterprise_business_hours_exceptions_rule
        FOREIGN KEY (tenant_id, application_id, environment_id, rule_id)
        REFERENCES enterprise_business_hours (tenant_id, application_id, environment_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_enterprise_business_hours_exceptions_kind CHECK (kind IN ('CLOSED', 'CUSTOM_HOURS'))
);

CREATE TABLE enterprise_business_hours_windows (
    id             UUID        NOT NULL,
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    rule_id        UUID        NOT NULL,
    day_of_week    SMALLINT,
    exception_date DATE,
    start_minute   SMALLINT    NOT NULL,
    end_minute     SMALLINT    NOT NULL,
    CONSTRAINT pk_enterprise_business_hours_windows PRIMARY KEY (id),
    CONSTRAINT fk_enterprise_business_hours_windows_rule
        FOREIGN KEY (tenant_id, application_id, environment_id, rule_id)
        REFERENCES enterprise_business_hours (tenant_id, application_id, environment_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_enterprise_business_hours_windows_exception
        FOREIGN KEY (rule_id, exception_date)
        REFERENCES enterprise_business_hours_exceptions (rule_id, exception_date) ON DELETE CASCADE,
    -- Either a weekly window or a custom-hours window of an exception date.
    CONSTRAINT ck_enterprise_business_hours_windows_owner
        CHECK ((day_of_week IS NULL) <> (exception_date IS NULL)),
    CONSTRAINT ck_enterprise_business_hours_windows_day
        CHECK (day_of_week IS NULL OR day_of_week BETWEEN 1 AND 7),
    CONSTRAINT ck_enterprise_business_hours_windows_minutes
        CHECK (start_minute BETWEEN 0 AND 1439 AND end_minute BETWEEN 1 AND 1440
               AND start_minute <> end_minute),
    -- A weekly window may cross midnight (end < start); an exception date's never does.
    CONSTRAINT ck_enterprise_business_hours_windows_exception_day
        CHECK (exception_date IS NULL OR end_minute > start_minute)
);
CREATE INDEX idx_enterprise_business_hours_windows_rule
    ON enterprise_business_hours_windows (rule_id);

CREATE TABLE enterprise_public_contacts (
    id                       UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    label                    VARCHAR(100) NOT NULL,
    contact_reference        UUID         NOT NULL,
    privacy_relay_session_id UUID         NOT NULL,
    status                   TEXT         NOT NULL,
    version                  BIGINT       NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    disabled_at              TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_public_contacts PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_public_contacts_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_public_contacts_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_public_contacts_label CHECK (length(btrim(label)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_public_contacts_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_public_contacts_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_public_contacts_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_public_contacts_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_public_contacts_active_reference
    ON enterprise_public_contacts (environment_id, contact_reference) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_public_contacts_scope
    ON enterprise_public_contacts (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_public_contacts_immutable
    BEFORE UPDATE ON enterprise_public_contacts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'contact_reference',
        'privacy_relay_session_id', 'created_at');
CREATE TRIGGER trg_enterprise_public_contacts_version
    BEFORE UPDATE ON enterprise_public_contacts
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

CREATE OR REPLACE FUNCTION enterprise_destination_exists(
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
        WHEN 'RING_GROUP' THEN EXISTS (SELECT 1 FROM enterprise_ring_groups g
            WHERE g.tenant_id = t AND g.application_id = a AND g.environment_id = e AND g.id = target)
        WHEN 'QUEUE' THEN EXISTS (SELECT 1 FROM enterprise_call_queues q
            WHERE q.tenant_id = t AND q.application_id = a AND q.environment_id = e AND q.id = target)
        WHEN 'IVR' THEN EXISTS (SELECT 1 FROM enterprise_ivr_flows f
            WHERE f.tenant_id = t AND f.application_id = a AND f.environment_id = e AND f.id = target)
        WHEN 'SCHEDULE' THEN EXISTS (SELECT 1 FROM enterprise_business_hours h
            WHERE h.tenant_id = t AND h.application_id = a AND h.environment_id = e AND h.id = target)
        WHEN 'PUBLIC_CONTACT' THEN EXISTS (SELECT 1 FROM enterprise_public_contacts c
            WHERE c.tenant_id = t AND c.application_id = a AND c.environment_id = e AND c.id = target)
        ELSE FALSE
    END
$$;

CREATE TABLE enterprise_forwarding_rules (
    id               UUID        NOT NULL,
    tenant_id        UUID        NOT NULL,
    application_id   UUID        NOT NULL,
    environment_id   UUID        NOT NULL,
    principal_id     UUID        NOT NULL,
    condition        TEXT        NOT NULL,
    destination_type TEXT        NOT NULL,
    destination_id   UUID        NOT NULL,
    status           TEXT        NOT NULL,
    version          BIGINT      NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL,
    disabled_at      TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_forwarding_rules PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_forwarding_rules_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_forwarding_rules_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_forwarding_rules_condition
        CHECK (condition IN ('ALWAYS', 'BUSY', 'NO_ANSWER', 'UNAVAILABLE')),
    CONSTRAINT ck_enterprise_forwarding_rules_destination_type CHECK (destination_type IN (
        'ENTERPRISE_MEMBER', 'PRINCIPAL', 'EXTENSION', 'RING_GROUP', 'QUEUE', 'IVR', 'VOICEMAIL',
        'PUBLIC_CONTACT')),
    -- Forwarding to oneself is a loop by construction.
    CONSTRAINT ck_enterprise_forwarding_rules_self CHECK (NOT (
        destination_type IN ('ENTERPRISE_MEMBER', 'PRINCIPAL') AND destination_id = principal_id)),
    CONSTRAINT ck_enterprise_forwarding_rules_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_forwarding_rules_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_forwarding_rules_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_forwarding_rules_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_forwarding_rules_active
    ON enterprise_forwarding_rules (principal_id, condition) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_forwarding_rules_scope
    ON enterprise_forwarding_rules (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_forwarding_rules_immutable
    BEFORE UPDATE ON enterprise_forwarding_rules
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'principal_id', 'condition',
        'created_at');
CREATE TRIGGER trg_enterprise_forwarding_rules_version
    BEFORE UPDATE ON enterprise_forwarding_rules
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_enterprise_forwarding_rules_destination
    BEFORE INSERT OR UPDATE ON enterprise_forwarding_rules
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('destination_type', 'destination_id');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS enterprise_forwarding_rules;
CREATE OR REPLACE FUNCTION enterprise_destination_exists(
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
        WHEN 'RING_GROUP' THEN EXISTS (SELECT 1 FROM enterprise_ring_groups g
            WHERE g.tenant_id = t AND g.application_id = a AND g.environment_id = e AND g.id = target)
        WHEN 'QUEUE' THEN EXISTS (SELECT 1 FROM enterprise_call_queues q
            WHERE q.tenant_id = t AND q.application_id = a AND q.environment_id = e AND q.id = target)
        WHEN 'IVR' THEN EXISTS (SELECT 1 FROM enterprise_ivr_flows f
            WHERE f.tenant_id = t AND f.application_id = a AND f.environment_id = e AND f.id = target)
        ELSE FALSE
    END
$$;
DROP TABLE IF EXISTS enterprise_public_contacts;
DROP TABLE IF EXISTS enterprise_business_hours_windows;
DROP TABLE IF EXISTS enterprise_business_hours_exceptions;
DROP TABLE IF EXISTS enterprise_business_hours;
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
