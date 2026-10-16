//! IVR flows (Part F §57, §65–§75).
//!
//! * `enterprise_ivr_flows` — a named flow of the scope and its ACTIVE published version.
//! * `enterprise_ivr_flow_versions` — immutable, validated, declarative graphs (typed
//!   nodes: PROMPT, MENU, ROUTE_*, HANGUP; built-in prompts only; never a script or a
//!   dialplan fragment). A Call keeps the version it started with even after a newer
//!   version is activated. Append-only.
//! * `enterprise_ivr_flow_destinations` — every destination a version routes to, as
//!   typed rows the shared destination guard proves exist in the SAME scope (a version
//!   can never route across tenants, even from a buggy writer).
//! * `enterprise_ivr_sessions` — the per-Call IVR business state: the current node, the
//!   retry and step counters (bounded), the input deadline (restart-safe timers) and the
//!   selected branch (a node id — never the digits pressed).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE enterprise_ivr_flows (
    id                UUID         NOT NULL,
    tenant_id         UUID         NOT NULL,
    application_id    UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    name              VARCHAR(100) NOT NULL,
    active_version_id UUID,
    status            TEXT         NOT NULL,
    version           BIGINT       NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL,
    updated_at        TIMESTAMPTZ  NOT NULL,
    disabled_at       TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_ivr_flows PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_ivr_flows_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_ivr_flows_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_ivr_flows_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_ivr_flows_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_ivr_flows_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_ivr_flows_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_ivr_flows_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_ivr_flows_active_name
    ON enterprise_ivr_flows (environment_id, lower(name)) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_ivr_flows_scope
    ON enterprise_ivr_flows (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_ivr_flows_immutable
    BEFORE UPDATE ON enterprise_ivr_flows
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_ivr_flows_version
    BEFORE UPDATE ON enterprise_ivr_flows
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

CREATE TABLE enterprise_ivr_flow_versions (
    id             UUID        NOT NULL,
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    flow_id        UUID        NOT NULL,
    version_number INTEGER     NOT NULL,
    graph          JSONB       NOT NULL,
    node_count     SMALLINT    NOT NULL,
    start_node     VARCHAR(32) NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_enterprise_ivr_flow_versions PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_ivr_flow_versions_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_enterprise_ivr_flow_versions_number UNIQUE (flow_id, version_number),
    CONSTRAINT fk_enterprise_ivr_flow_versions_flow
        FOREIGN KEY (tenant_id, application_id, environment_id, flow_id)
        REFERENCES enterprise_ivr_flows (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_ivr_flow_versions_number CHECK (version_number BETWEEN 1 AND 100000),
    CONSTRAINT ck_enterprise_ivr_flow_versions_graph
        CHECK (jsonb_typeof(graph) = 'object' AND octet_length(graph::text) <= 65536),
    CONSTRAINT ck_enterprise_ivr_flow_versions_nodes CHECK (node_count BETWEEN 1 AND 32),
    CONSTRAINT ck_enterprise_ivr_flow_versions_start CHECK (start_node ~ '^[a-z][a-z0-9_]{0,31}$')
);
CREATE TRIGGER trg_enterprise_ivr_flow_versions_append_only
    BEFORE UPDATE OR DELETE ON enterprise_ivr_flow_versions
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

ALTER TABLE enterprise_ivr_flows
    ADD CONSTRAINT fk_enterprise_ivr_flows_active_version
        FOREIGN KEY (tenant_id, application_id, environment_id, active_version_id)
        REFERENCES enterprise_ivr_flow_versions (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT;

CREATE TABLE enterprise_ivr_flow_destinations (
    tenant_id        UUID        NOT NULL,
    application_id   UUID        NOT NULL,
    environment_id   UUID        NOT NULL,
    flow_version_id  UUID        NOT NULL,
    node_id          VARCHAR(32) NOT NULL,
    destination_type TEXT        NOT NULL,
    destination_id   UUID        NOT NULL,
    CONSTRAINT pk_enterprise_ivr_flow_destinations PRIMARY KEY (flow_version_id, node_id),
    CONSTRAINT fk_enterprise_ivr_flow_destinations_version
        FOREIGN KEY (tenant_id, application_id, environment_id, flow_version_id)
        REFERENCES enterprise_ivr_flow_versions (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_ivr_flow_destinations_type CHECK (destination_type IN (
        'EXTENSION', 'RING_GROUP', 'QUEUE', 'VOICEMAIL', 'SCHEDULE', 'IVR'))
);
CREATE TRIGGER trg_enterprise_ivr_flow_destinations_guard
    BEFORE INSERT OR UPDATE ON enterprise_ivr_flow_destinations
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_destination('destination_type', 'destination_id');
CREATE TRIGGER trg_enterprise_ivr_flow_destinations_append_only
    BEFORE UPDATE OR DELETE ON enterprise_ivr_flow_destinations
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();

CREATE TABLE enterprise_ivr_sessions (
    id               UUID        NOT NULL,
    tenant_id        UUID        NOT NULL,
    application_id   UUID        NOT NULL,
    environment_id   UUID        NOT NULL,
    call_id          UUID        NOT NULL,
    flow_id          UUID        NOT NULL,
    flow_version_id  UUID        NOT NULL,
    input_source    TEXT        NOT NULL,
    state            TEXT        NOT NULL,
    current_node     VARCHAR(32) NOT NULL,
    retries          SMALLINT    NOT NULL,
    steps            SMALLINT    NOT NULL,
    awaiting_input   BOOLEAN     NOT NULL,
    input_deadline   TIMESTAMPTZ,
    selected_branch  VARCHAR(32),
    last_prompts     TEXT[]      NOT NULL,
    end_reason       TEXT,
    started_at       TIMESTAMPTZ NOT NULL,
    ended_at         TIMESTAMPTZ,
    version          BIGINT      NOT NULL,
    CONSTRAINT pk_enterprise_ivr_sessions PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_ivr_sessions_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_ivr_sessions_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_ivr_sessions_flow
        FOREIGN KEY (tenant_id, application_id, environment_id, flow_id)
        REFERENCES enterprise_ivr_flows (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_ivr_sessions_version
        FOREIGN KEY (tenant_id, application_id, environment_id, flow_version_id)
        REFERENCES enterprise_ivr_flow_versions (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_ivr_sessions_source CHECK (input_source IN ('APPLICATION', 'SIP')),
    CONSTRAINT ck_enterprise_ivr_sessions_state
        CHECK (state IN ('ACTIVE', 'ROUTED', 'COMPLETED', 'ABANDONED', 'FAILED')),
    CONSTRAINT ck_enterprise_ivr_sessions_node CHECK (current_node ~ '^[a-z][a-z0-9_]{0,31}$'),
    CONSTRAINT ck_enterprise_ivr_sessions_branch
        CHECK (selected_branch IS NULL OR selected_branch ~ '^[a-z][a-z0-9_]{0,31}$'),
    CONSTRAINT ck_enterprise_ivr_sessions_prompts CHECK (cardinality(last_prompts) <= 8),
    CONSTRAINT ck_enterprise_ivr_sessions_bounds CHECK (retries BETWEEN 0 AND 10 AND steps BETWEEN 0 AND 64),
    CONSTRAINT ck_enterprise_ivr_sessions_waiting
        CHECK (NOT awaiting_input OR (state = 'ACTIVE' AND input_deadline IS NOT NULL)),
    CONSTRAINT ck_enterprise_ivr_sessions_end
        CHECK ((state = 'ACTIVE') = (ended_at IS NULL) AND (state = 'ACTIVE') = (end_reason IS NULL)),
    CONSTRAINT ck_enterprise_ivr_sessions_end_reason CHECK (end_reason IS NULL OR end_reason IN (
        'ROUTED', 'HANGUP_NODE', 'CALLER_HUNG_UP', 'CALL_ENDED', 'STEP_BUDGET_EXHAUSTED',
        'DESTINATION_UNAVAILABLE')),
    CONSTRAINT ck_enterprise_ivr_sessions_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_ivr_sessions_timestamps CHECK (ended_at IS NULL OR ended_at >= started_at)
);
CREATE UNIQUE INDEX uq_enterprise_ivr_sessions_active_call
    ON enterprise_ivr_sessions (call_id) WHERE state = 'ACTIVE';
-- Due menus (input timer) and HANGUP prompts being played (farewell timer).
CREATE INDEX idx_enterprise_ivr_sessions_deadline
    ON enterprise_ivr_sessions (input_deadline) WHERE state = 'ACTIVE';
CREATE INDEX idx_enterprise_ivr_sessions_call ON enterprise_ivr_sessions (call_id);
CREATE TRIGGER trg_enterprise_ivr_sessions_immutable
    BEFORE UPDATE ON enterprise_ivr_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'call_id', 'flow_id',
        'flow_version_id', 'input_source', 'started_at');
CREATE TRIGGER trg_enterprise_ivr_sessions_version
    BEFORE UPDATE ON enterprise_ivr_sessions
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();
CREATE TRIGGER trg_enterprise_ivr_sessions_final
    BEFORE UPDATE ON enterprise_ivr_sessions
    FOR EACH ROW EXECUTE FUNCTION enterprise_queue_final_state('state', 'ACTIVE');

-- An IVR may end the Call itself (HANGUP node): the business outcome says so.
ALTER TABLE enterprise_call_contexts
    DROP CONSTRAINT ck_enterprise_call_contexts_outcome,
    ADD CONSTRAINT ck_enterprise_call_contexts_outcome CHECK (outcome IS NULL OR outcome IN (
        'ANSWERED', 'VOICEMAIL', 'NO_ANSWER', 'DECLINED', 'ABANDONED', 'FAILED', 'FORWARDED_PUBLIC',
        'BUSY', 'COMPLETED_IN_IVR')),
    ADD COLUMN ivr_session_id UUID,
    ADD CONSTRAINT fk_enterprise_call_contexts_ivr_session
        FOREIGN KEY (tenant_id, application_id, environment_id, ivr_session_id)
        REFERENCES enterprise_ivr_sessions (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT;

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
"#;

const DOWN: &str = r#"
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
        ELSE FALSE
    END
$$;
ALTER TABLE enterprise_call_contexts
    DROP CONSTRAINT IF EXISTS fk_enterprise_call_contexts_ivr_session,
    DROP COLUMN IF EXISTS ivr_session_id,
    DROP CONSTRAINT ck_enterprise_call_contexts_outcome,
    ADD CONSTRAINT ck_enterprise_call_contexts_outcome CHECK (outcome IS NULL OR outcome IN (
        'ANSWERED', 'VOICEMAIL', 'NO_ANSWER', 'DECLINED', 'ABANDONED', 'FAILED', 'FORWARDED_PUBLIC',
        'BUSY'));
DROP TABLE IF EXISTS enterprise_ivr_sessions;
DROP TABLE IF EXISTS enterprise_ivr_flow_destinations;
ALTER TABLE enterprise_ivr_flows DROP CONSTRAINT IF EXISTS fk_enterprise_ivr_flows_active_version;
DROP TABLE IF EXISTS enterprise_ivr_flow_versions;
DROP TABLE IF EXISTS enterprise_ivr_flows;
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
