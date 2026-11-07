//! Blueprint v1.0 Section 15 — organization provisioning, tenant-name sync and the
//! platform-level operator audit.
//!
//! * `devplat_organization_provisioning` (ORG-014): one row per self-service organization
//!   creation, written BEFORE Platform Access is asked for the tenant. Its id derives the
//!   tenant's creation slug, so the tenant can always be found again. A creation whose
//!   local commit never happened stays PENDING; the worker reconciles it — the orphaned
//!   EXTERNAL tenant is suspended in Platform Access (COMPENSATED), or the row is closed
//!   ABANDONED when no tenant was created. It holds an opaque creation id, the requesting
//!   actor reference, the resulting tenant id and a bounded outcome code only.
//! * `devplat_organizations.platform_name_sync_pending` (ORG-013): set in the rename's
//!   transaction, cleared once Platform Access holds the same display name; the worker
//!   retries pending rows. Existing self-service organizations start pending so any name
//!   drift from before this migration is repaired once.
//! * `devplat_operator_audit` (PLT-009): operator commands that are not about one
//!   organization (commercial plan publication / retirement, usage rollup rebuilds,
//!   backfills, stream resets) — tenant-scoped operator commands keep writing OPERATOR
//!   events into `devplat_activity_events`. Append-only, safe summary only (the same
//!   refused keys as activity). A partial index serves the operator read of both.
//!
//! Additive and rollback-safe: the down migration drops only what this one created.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_organization_provisioning (
    id            UUID         NOT NULL,
    requested_by  VARCHAR(100) NOT NULL,
    state         VARCHAR(16)  NOT NULL,
    tenant_id     UUID,
    attempts      INTEGER      NOT NULL DEFAULT 0,
    outcome_code  VARCHAR(64),
    created_at    TIMESTAMPTZ  NOT NULL,
    updated_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_organization_provisioning PRIMARY KEY (id),
    CONSTRAINT ck_devplat_organization_provisioning_state
        CHECK (state IN ('PENDING', 'COMPLETED', 'COMPENSATED', 'ABANDONED')),
    CONSTRAINT ck_devplat_organization_provisioning_tenant
        CHECK (state NOT IN ('COMPLETED', 'COMPENSATED') OR tenant_id IS NOT NULL),
    CONSTRAINT ck_devplat_organization_provisioning_outcome
        CHECK (outcome_code IS NULL OR outcome_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_organization_provisioning_attempts CHECK (attempts >= 0)
);
CREATE INDEX ix_devplat_organization_provisioning_pending
    ON devplat_organization_provisioning (created_at) WHERE state = 'PENDING';

ALTER TABLE devplat_organizations
    ADD COLUMN platform_name_sync_pending BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE devplat_organizations SET platform_name_sync_pending = TRUE
    WHERE created_by NOT LIKE 'operator:%';
CREATE INDEX ix_devplat_organizations_name_sync
    ON devplat_organizations (updated_at) WHERE platform_name_sync_pending;

CREATE TABLE devplat_operator_audit (
    id              UUID          NOT NULL,
    operator        VARCHAR(100)  NOT NULL,
    action          VARCHAR(64)   NOT NULL,
    target_type     VARCHAR(32)   NOT NULL,
    target_id       VARCHAR(64)   NOT NULL,
    request_id      VARCHAR(64)   NOT NULL,
    correlation_id  VARCHAR(64)   NOT NULL,
    summary         JSONB         NOT NULL DEFAULT '{}'::jsonb,
    occurred_at     TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_operator_audit PRIMARY KEY (id),
    CONSTRAINT ck_devplat_operator_audit_operator CHECK (operator ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$'),
    CONSTRAINT ck_devplat_operator_audit_action CHECK (action ~ '^[a-z][a-z_]*\.[a-z][a-z_.]*$'),
    CONSTRAINT ck_devplat_operator_audit_summary CHECK (
        jsonb_typeof(summary) = 'object' AND octet_length(summary::text) <= 2048
        AND NOT (summary ?| ARRAY['secret', 'signingSecret', 'credential', 'password', 'token', 'apiKey',
                                  'verifier', 'privateKey', 'phoneNumber', 'email', 'contact']))
);
CREATE INDEX ix_devplat_operator_audit_recent ON devplat_operator_audit (occurred_at DESC, id DESC);
CREATE TRIGGER tr_devplat_operator_audit_append_only BEFORE UPDATE OR DELETE ON devplat_operator_audit
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE INDEX ix_devplat_activity_events_operator
    ON devplat_activity_events (occurred_at DESC, id DESC) WHERE actor_kind = 'OPERATOR';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS ix_devplat_activity_events_operator;
DROP TABLE IF EXISTS devplat_operator_audit;
DROP INDEX IF EXISTS ix_devplat_organizations_name_sync;
ALTER TABLE devplat_organizations DROP COLUMN IF EXISTS platform_name_sync_pending;
DROP TABLE IF EXISTS devplat_organization_provisioning;
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
