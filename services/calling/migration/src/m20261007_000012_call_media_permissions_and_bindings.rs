//! Server-authoritative media permissions, the Calling-private media binding and the
//! operational route lifecycle (Part C §21, §23, §24).
//!
//! * `call_participant_media_permissions` — one row per participant, the permissions
//!   every media credential and every provider permission update is derived from. A
//!   VOICE Call can never hold a video publish permission: the row is pinned to the
//!   Call's kind by composite foreign key and a CHECK refuses `can_publish_video` unless
//!   the kind is VIDEO. `revision` increases on every change (audit: CALL_MEDIA_
//!   PERMISSIONS_UPDATED events carry before / after).
//! * `call_media_sessions` — the Calling-private binding between a Call's APPLICATION
//!   route / RTC leg and the media provider resource executing it. It is NOT the
//!   canonical leg: the provider adapter name and the provider's own resource reference
//!   live only here, are never part of the public API or of customer events, and are never
//!   a Call id. The canonical resource name is derived from the Call id
//!   (`cfc-<call uuid without hyphens>`, CHECK-enforced), so retries can never create a
//!   second resource and no client can choose it. One binding per Call.
//! * Routes gain the operational terminal states EXHAUSTED (every target path ended
//!   without an answer) and CANCELLED (the Call was cancelled before the route
//!   completed); SELECTED → ACTIVE → ENDED / EXHAUSTED / FAILED / CANCELLED / SUPERSEDED.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Composite target: (scope, call, kind) lets child rows depend on the Call's kind.
ALTER TABLE calls
    ADD CONSTRAINT uq_calls_owner_kind_id UNIQUE (tenant_id, application_id, environment_id, id, call_kind);

-- ---------------------------------------------------------------------------
-- Operational route lifecycle.
-- ---------------------------------------------------------------------------
ALTER TABLE call_routes
    DROP CONSTRAINT ck_call_routes_state,
    DROP CONSTRAINT ck_call_routes_ended,
    ADD CONSTRAINT ck_call_routes_state CHECK (state IN (
        'SELECTED', 'ACTIVE', 'ENDED', 'EXHAUSTED', 'FAILED', 'CANCELLED', 'SUPERSEDED')),
    ADD CONSTRAINT ck_call_routes_ended CHECK (
        (state IN ('ENDED', 'EXHAUSTED', 'FAILED', 'CANCELLED', 'SUPERSEDED')) = (ended_at IS NOT NULL));
DROP TRIGGER trg_call_routes_state_transition ON call_routes;
CREATE TRIGGER trg_call_routes_state_transition
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('state',
        'SELECTED>ACTIVE', 'SELECTED>ENDED', 'SELECTED>EXHAUSTED', 'SELECTED>FAILED',
        'SELECTED>CANCELLED', 'SELECTED>SUPERSEDED',
        'ACTIVE>ENDED', 'ACTIVE>EXHAUSTED', 'ACTIVE>FAILED', 'ACTIVE>CANCELLED', 'ACTIVE>SUPERSEDED');

-- ---------------------------------------------------------------------------
-- Per-participant, server-authoritative media permissions.
-- ---------------------------------------------------------------------------
CREATE TABLE call_participant_media_permissions (
    id                UUID PRIMARY KEY,
    call_id           UUID        NOT NULL,
    participant_id    UUID        NOT NULL,
    tenant_id         UUID        NOT NULL,
    application_id    UUID        NOT NULL,
    environment_id    UUID        NOT NULL,
    call_kind         TEXT        NOT NULL,
    revision          INTEGER     NOT NULL,
    can_publish_audio BOOLEAN     NOT NULL,
    can_publish_video BOOLEAN     NOT NULL,
    can_publish_data  BOOLEAN     NOT NULL,
    can_subscribe     BOOLEAN     NOT NULL,
    policy_version    VARCHAR(64) NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_participant_media_permissions_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, call_kind)
        REFERENCES calls (tenant_id, application_id, environment_id, id, call_kind) ON DELETE RESTRICT,
    CONSTRAINT fk_call_participant_media_permissions_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_participant_media_permissions_participant UNIQUE (call_id, participant_id),
    -- A voice call never carries a video publish permission.
    CONSTRAINT ck_call_participant_media_permissions_kind
        CHECK (call_kind = 'VIDEO' OR NOT can_publish_video),
    CONSTRAINT ck_call_participant_media_permissions_revision CHECK (revision >= 1),
    CONSTRAINT ck_call_participant_media_permissions_policy_version
        CHECK (policy_version ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT ck_call_participant_media_permissions_timestamps CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_call_participant_media_permissions_immutable
    BEFORE UPDATE ON call_participant_media_permissions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'participant_id', 'tenant_id', 'application_id', 'environment_id',
        'call_kind', 'created_at');
CREATE TRIGGER trg_call_participant_media_permissions_monotonic
    BEFORE UPDATE ON call_participant_media_permissions
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_decrease('revision');
CREATE TRIGGER trg_call_participant_media_permissions_no_delete
    BEFORE DELETE ON call_participant_media_permissions
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Calling-private media binding (route / RTC leg ↔ provider resource).
-- ---------------------------------------------------------------------------
CREATE TABLE call_media_sessions (
    id                    UUID PRIMARY KEY,
    call_id               UUID        NOT NULL,
    route_id              UUID        NOT NULL,
    leg_id                UUID        NOT NULL,
    tenant_id             UUID        NOT NULL,
    application_id        UUID        NOT NULL,
    environment_id        UUID        NOT NULL,
    -- Internal: which adapter executes the resource, and the provider's own reference to
    -- it. Never public, never a Call id, never in customer events.
    provider_adapter      VARCHAR(32) NOT NULL,
    provider_resource_ref VARCHAR(128),
    -- Canonical, provider-neutral, Call-derived resource name.
    media_resource_name   VARCHAR(40) NOT NULL,
    status                TEXT        NOT NULL,
    max_participants      INTEGER     NOT NULL,
    enforcement_pending   BOOLEAN     NOT NULL,
    failure_count         INTEGER     NOT NULL,
    last_failure          TEXT,
    created_at            TIMESTAMPTZ NOT NULL,
    ensured_at            TIMESTAMPTZ,
    terminated_at         TIMESTAMPTZ,
    last_reconciled_at    TIMESTAMPTZ,
    next_reconcile_at     TIMESTAMPTZ NOT NULL,
    updated_at            TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_media_sessions_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_media_sessions_route
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, route_id)
        REFERENCES call_routes (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_media_sessions_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    -- One canonical media resource per Call.
    CONSTRAINT uq_call_media_sessions_call UNIQUE (call_id),
    CONSTRAINT uq_call_media_sessions_name UNIQUE (media_resource_name),
    CONSTRAINT uq_call_media_sessions_leg UNIQUE (leg_id),
    CONSTRAINT ck_call_media_sessions_name
        CHECK (media_resource_name = 'cfc-' || replace(call_id::text, '-', '')),
    CONSTRAINT ck_call_media_sessions_adapter CHECK (provider_adapter ~ '^[a-z][a-z0-9_-]{1,31}$'),
    CONSTRAINT ck_call_media_sessions_resource_ref
        CHECK (provider_resource_ref IS NULL OR provider_resource_ref ~ '^[A-Za-z0-9_.:-]{1,128}$'),
    CONSTRAINT ck_call_media_sessions_status
        CHECK (status IN ('ENSURING', 'ACTIVE', 'TERMINATING', 'TERMINATED')),
    CONSTRAINT ck_call_media_sessions_ensured CHECK (
        (status <> 'ENSURING' OR ensured_at IS NULL) AND (status <> 'ACTIVE' OR ensured_at IS NOT NULL)),
    CONSTRAINT ck_call_media_sessions_terminated CHECK ((status = 'TERMINATED') = (terminated_at IS NOT NULL)),
    CONSTRAINT ck_call_media_sessions_max_participants CHECK (max_participants BETWEEN 2 AND 1000),
    CONSTRAINT ck_call_media_sessions_failure_count CHECK (failure_count >= 0),
    CONSTRAINT ck_call_media_sessions_last_failure CHECK (last_failure IS NULL OR last_failure IN (
        'TIMEOUT', 'UNAVAILABLE', 'REJECTED', 'NOT_FOUND', 'UNKNOWN')),
    CONSTRAINT ck_call_media_sessions_timestamps CHECK (
        (ensured_at IS NULL OR ensured_at >= created_at)
        AND (terminated_at IS NULL OR terminated_at >= created_at)
        AND updated_at >= created_at)
);
-- Reconciliation scans only bindings that are not terminated, in due order.
CREATE INDEX idx_call_media_sessions_due
    ON call_media_sessions (next_reconcile_at) WHERE status <> 'TERMINATED';
CREATE TRIGGER trg_call_media_sessions_immutable
    BEFORE UPDATE ON call_media_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'route_id', 'leg_id', 'tenant_id', 'application_id', 'environment_id',
        'provider_adapter', 'media_resource_name', 'max_participants', 'created_at');
CREATE TRIGGER trg_call_media_sessions_set_once
    BEFORE UPDATE ON call_media_sessions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('ensured_at', 'terminated_at');
CREATE TRIGGER trg_call_media_sessions_status_transition
    BEFORE UPDATE ON call_media_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'ENSURING>ACTIVE', 'ENSURING>TERMINATING', 'ENSURING>TERMINATED',
        'ACTIVE>TERMINATING', 'ACTIVE>TERMINATED', 'TERMINATING>TERMINATED');
CREATE TRIGGER trg_call_media_sessions_no_delete
    BEFORE DELETE ON call_media_sessions
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_media_sessions;
DROP TABLE IF EXISTS call_participant_media_permissions;
DROP TRIGGER IF EXISTS trg_call_routes_state_transition ON call_routes;
CREATE TRIGGER trg_call_routes_state_transition
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('state',
        'SELECTED>ACTIVE', 'SELECTED>ENDED', 'SELECTED>FAILED', 'SELECTED>SUPERSEDED',
        'ACTIVE>ENDED', 'ACTIVE>FAILED', 'ACTIVE>SUPERSEDED');
ALTER TABLE call_routes
    DROP CONSTRAINT IF EXISTS ck_call_routes_ended,
    DROP CONSTRAINT IF EXISTS ck_call_routes_state,
    ADD CONSTRAINT ck_call_routes_state CHECK (state IN ('SELECTED', 'ACTIVE', 'ENDED', 'FAILED', 'SUPERSEDED')),
    ADD CONSTRAINT ck_call_routes_ended CHECK ((state IN ('ENDED', 'FAILED', 'SUPERSEDED')) = (ended_at IS NOT NULL));
ALTER TABLE calls DROP CONSTRAINT IF EXISTS uq_calls_owner_kind_id;
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
