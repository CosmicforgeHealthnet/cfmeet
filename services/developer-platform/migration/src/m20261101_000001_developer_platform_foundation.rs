//! Developer Platform foundation (Part VI §§1–5).
//!
//! * `devplat_environment_refs` — the anchor every later Developer Platform resource
//!   (Sections 50+) hangs from: ONE row per Platform Access application environment the
//!   Developer Platform has served, holding only the canonical Platform Access ids
//!   (tenant → application → environment) after Platform Access re-validated them. The
//!   identity hierarchy itself is never copied.
//! * `devplat_worker_heartbeats` — liveness of worker instances (instance id, runtime
//!   environment, first / last beat).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_environment_refs (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    first_seen_at       TIMESTAMPTZ  NOT NULL,
    last_seen_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_environment_refs PRIMARY KEY (id),
    CONSTRAINT uq_devplat_environment_refs_environment UNIQUE (environment_id),
    CONSTRAINT uq_devplat_environment_refs_chain UNIQUE (tenant_id, application_id, environment_id),
    CONSTRAINT ck_devplat_environment_refs_seen CHECK (last_seen_at >= first_seen_at)
);
COMMENT ON TABLE devplat_environment_refs IS
    'Canonical Platform Access ids only (Platform Access is the identity authority); no identity attributes, scopes or credentials.';

-- The chain of an environment never changes: Platform Access ids are immutable.
CREATE FUNCTION devplat_environment_refs_immutable() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tenant_id <> OLD.tenant_id
       OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id
       OR NEW.first_seen_at <> OLD.first_seen_at THEN
        RAISE EXCEPTION 'devplat_environment_refs: canonical references are immutable'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_environment_refs_immutable
    BEFORE UPDATE ON devplat_environment_refs
    FOR EACH ROW EXECUTE FUNCTION devplat_environment_refs_immutable();

CREATE TABLE devplat_worker_heartbeats (
    instance_id         VARCHAR(64)  NOT NULL,
    runtime_environment VARCHAR(16)  NOT NULL,
    started_at          TIMESTAMPTZ  NOT NULL,
    last_seen_at        TIMESTAMPTZ  NOT NULL,
    beats               BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_worker_heartbeats PRIMARY KEY (instance_id),
    CONSTRAINT ck_devplat_worker_heartbeats_instance CHECK (instance_id ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    CONSTRAINT ck_devplat_worker_heartbeats_runtime CHECK (runtime_environment IN ('local', 'test', 'staging', 'production')),
    CONSTRAINT ck_devplat_worker_heartbeats_seen CHECK (last_seen_at >= started_at),
    CONSTRAINT ck_devplat_worker_heartbeats_beats CHECK (beats >= 1)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_worker_heartbeats;
DROP TRIGGER IF EXISTS tr_devplat_environment_refs_immutable ON devplat_environment_refs;
DROP FUNCTION IF EXISTS devplat_environment_refs_immutable();
DROP TABLE IF EXISTS devplat_environment_refs;
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
