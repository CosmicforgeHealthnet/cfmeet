//! Routing Engine persistence (Part E §37–§38, §23–§24).
//!
//! * `call_routes` keeps its Part C identity, network class, state graph and bounded
//!   decision snapshot. It gains the canonical routing class (APP_RTC … ENTERPRISE_INTERNAL),
//!   the bounded provider-independent RoutePlan, its fingerprint, the selected candidate
//!   route reference (opaque Telephony route id, set once), the attempt budget, the plan
//!   validity and a version. Legacy APPLICATION routes are backfilled to APP_RTC.
//! * The same plan fingerprint can be active at most once per Call (`SELECTED` / `ACTIVE`):
//!   recomputing an identical selection never creates a duplicate active route.
//! * `call_routing_policies` holds the typed routing policy of one application
//!   environment (validated by the service; never arbitrary route JSON). No row = the
//!   restrictive default (application RTC only).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE call_routes
    ADD COLUMN routing_class      TEXT,
    ADD COLUMN route_plan         JSONB,
    ADD COLUMN plan_fingerprint   VARCHAR(64),
    ADD COLUMN selected_route_ref VARCHAR(64),
    ADD COLUMN max_attempts       SMALLINT,
    ADD COLUMN plan_expires_at    TIMESTAMPTZ,
    ADD COLUMN route_version      BIGINT NOT NULL DEFAULT 1;

UPDATE call_routes SET routing_class = 'APP_RTC' WHERE route_class = 'APPLICATION';

ALTER TABLE call_routes
    ADD CONSTRAINT ck_call_routes_routing_class CHECK (routing_class IS NULL OR routing_class IN (
        'APP_RTC', 'APP_TO_PSTN', 'PSTN_TO_APP', 'PSTN_RELAY', 'CALLBACK_RELAY', 'ENTERPRISE_INTERNAL')),
    ADD CONSTRAINT ck_call_routes_routing_network CHECK (routing_class IS NULL OR
        (routing_class = 'APP_RTC' AND route_class = 'APPLICATION') OR
        (routing_class IN ('APP_TO_PSTN', 'PSTN_TO_APP') AND route_class = 'MIXED') OR
        (routing_class IN ('PSTN_RELAY', 'CALLBACK_RELAY') AND route_class = 'PUBLIC_NETWORK') OR
        (routing_class = 'ENTERPRISE_INTERNAL' AND route_class = 'PRIVATE_NETWORK')),
    ADD CONSTRAINT ck_call_routes_plan CHECK (route_plan IS NULL OR (
        routing_class IS NOT NULL AND jsonb_typeof(route_plan) = 'object'
        AND octet_length(route_plan::text) <= 8192)),
    ADD CONSTRAINT ck_call_routes_plan_fingerprint
        CHECK ((plan_fingerprint IS NULL) = (route_plan IS NULL)
               AND (plan_fingerprint IS NULL OR plan_fingerprint ~ '^[0-9a-f]{64}$')),
    ADD CONSTRAINT ck_call_routes_selected_route_ref
        CHECK (selected_route_ref IS NULL OR selected_route_ref ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    ADD CONSTRAINT ck_call_routes_max_attempts CHECK (max_attempts IS NULL OR max_attempts BETWEEN 1 AND 4),
    ADD CONSTRAINT ck_call_routes_version CHECK (route_version >= 1);

CREATE UNIQUE INDEX uq_call_routes_active_plan ON call_routes (call_id, plan_fingerprint)
    WHERE plan_fingerprint IS NOT NULL AND state IN ('SELECTED', 'ACTIVE');
CREATE INDEX idx_call_routes_routing_class ON call_routes (routing_class, state);

CREATE TRIGGER trg_call_routes_plan_immutable
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'routing_class', 'route_plan', 'plan_fingerprint', 'max_attempts', 'plan_expires_at');
CREATE TRIGGER trg_call_routes_selected_ref_once
    BEFORE UPDATE ON call_routes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('selected_route_ref');

CREATE TABLE call_routing_policies (
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    policy         JSONB       NOT NULL,
    revision       INTEGER     NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_routing_policies PRIMARY KEY (environment_id),
    CONSTRAINT fk_call_routing_policies_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_call_routing_policies_policy
        CHECK (jsonb_typeof(policy) = 'object' AND octet_length(policy::text) <= 16384),
    CONSTRAINT ck_call_routing_policies_revision CHECK (revision >= 1),
    CONSTRAINT ck_call_routing_policies_timestamps CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_call_routing_policies_immutable
    BEFORE UPDATE ON call_routing_policies
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'tenant_id', 'application_id', 'environment_id', 'created_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_routing_policies;
DROP TRIGGER IF EXISTS trg_call_routes_selected_ref_once ON call_routes;
DROP TRIGGER IF EXISTS trg_call_routes_plan_immutable ON call_routes;
DROP INDEX IF EXISTS idx_call_routes_routing_class;
DROP INDEX IF EXISTS uq_call_routes_active_plan;
ALTER TABLE call_routes
    DROP CONSTRAINT IF EXISTS ck_call_routes_version,
    DROP CONSTRAINT IF EXISTS ck_call_routes_max_attempts,
    DROP CONSTRAINT IF EXISTS ck_call_routes_selected_route_ref,
    DROP CONSTRAINT IF EXISTS ck_call_routes_plan_fingerprint,
    DROP CONSTRAINT IF EXISTS ck_call_routes_plan,
    DROP CONSTRAINT IF EXISTS ck_call_routes_routing_network,
    DROP CONSTRAINT IF EXISTS ck_call_routes_routing_class,
    DROP COLUMN IF EXISTS route_version,
    DROP COLUMN IF EXISTS plan_expires_at,
    DROP COLUMN IF EXISTS max_attempts,
    DROP COLUMN IF EXISTS selected_route_ref,
    DROP COLUMN IF EXISTS plan_fingerprint,
    DROP COLUMN IF EXISTS route_plan,
    DROP COLUMN IF EXISTS routing_class;
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
