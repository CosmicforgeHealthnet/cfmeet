//! Workstream K: number routing grants.
//!
//! `number_routing_grants` records every grant Numbering signed (never the token itself):
//! its kind, the number, the ACTIVE assignment and the number's ROUTING REVISION it was
//! issued at, the scope, the audience and requester, the routing target (inbound) or the
//! single operation (outbound). A grant is consumed at most once (outbound only) and
//! revoked at most once; nothing else ever changes. Expired grants are purged after a
//! day (the only deletion).

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261023_000009_routing_grants"
    }
}

const UP: &str = r#"
CREATE TABLE number_routing_grants (
    id                 UUID         NOT NULL,
    token_id           VARCHAR(32)  NOT NULL,
    grant_kind         VARCHAR(20)  NOT NULL,
    number_resource_id UUID         NOT NULL,
    assignment_id      UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    routing_revision   BIGINT       NOT NULL,
    audience           VARCHAR(64)  NOT NULL,
    requested_by       VARCHAR(64)  NOT NULL,
    principal_id       UUID,
    target_kind        VARCHAR(40),
    target_ref         UUID,
    operation_ref      VARCHAR(128),
    issued_at          TIMESTAMPTZ  NOT NULL,
    expires_at         TIMESTAMPTZ  NOT NULL,
    consumed_at        TIMESTAMPTZ,
    consumed_by        VARCHAR(64),
    revoked_at         TIMESTAMPTZ,
    revoke_reason      VARCHAR(64),
    correlation_id     VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_routing_grants PRIMARY KEY (id),
    CONSTRAINT uq_number_routing_grants_token UNIQUE (token_id),
    CONSTRAINT fk_number_routing_grants_assignment FOREIGN KEY (assignment_id, tenant_id, application_id, environment_id, number_resource_id)
        REFERENCES dcn_assignments (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT ck_number_routing_grants_kind CHECK (grant_kind IN ('INBOUND_VOICE', 'INBOUND_SMS', 'CALLER_IDENTITY', 'SMS_SENDER')),
    CONSTRAINT ck_number_routing_grants_token CHECK (token_id ~ '^[0-9a-f]{32}$'),
    CONSTRAINT ck_number_routing_grants_shape CHECK (
        (grant_kind IN ('CALLER_IDENTITY', 'SMS_SENDER')
            AND operation_ref ~ '^[A-Za-z0-9._:-]{8,128}$' AND target_kind IS NULL AND target_ref IS NULL)
        OR (grant_kind IN ('INBOUND_VOICE', 'INBOUND_SMS')
            AND operation_ref IS NULL AND target_kind IS NOT NULL AND target_ref IS NOT NULL
            AND consumed_at IS NULL)),
    CONSTRAINT ck_number_routing_grants_lifetime CHECK (expires_at > issued_at AND expires_at <= issued_at + interval '300 seconds'),
    CONSTRAINT ck_number_routing_grants_revision CHECK (routing_revision >= 0),
    CONSTRAINT ck_number_routing_grants_consumed CHECK ((consumed_at IS NULL) = (consumed_by IS NULL)),
    CONSTRAINT ck_number_routing_grants_revoked CHECK ((revoked_at IS NULL) = (revoke_reason IS NULL)),
    CONSTRAINT ck_number_routing_grants_services CHECK (audience ~ '^cfmeet\.[a-z][a-z0-9-]{1,40}$'
        AND requested_by ~ '^cfmeet\.[a-z][a-z0-9-]{1,40}$' AND audience <> 'cfmeet.numbering')
);
-- One grant per outbound operation of a scope (an operation is authorized once).
CREATE UNIQUE INDEX uq_number_routing_grants_operation
    ON number_routing_grants (grant_kind, tenant_id, application_id, environment_id, operation_ref)
    WHERE operation_ref IS NOT NULL;
CREATE INDEX ix_number_routing_grants_live ON number_routing_grants (number_resource_id) WHERE revoked_at IS NULL;
CREATE INDEX ix_number_routing_grants_expiry ON number_routing_grants (expires_at);

CREATE FUNCTION number_guard_routing_grant() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.expires_at >= now() - interval '1 day' THEN
            RAISE EXCEPTION 'a routing grant is kept until a day after it expired' USING ERRCODE = '42501';
        END IF;
        RETURN OLD;
    END IF;
    IF (to_jsonb(NEW) - 'consumed_at' - 'consumed_by' - 'revoked_at' - 'revoke_reason')
       IS DISTINCT FROM (to_jsonb(OLD) - 'consumed_at' - 'consumed_by' - 'revoked_at' - 'revoke_reason') THEN
        RAISE EXCEPTION 'a routing grant is immutable' USING ERRCODE = '42501';
    END IF;
    IF OLD.consumed_at IS NOT NULL AND (NEW.consumed_at, NEW.consumed_by) IS DISTINCT FROM (OLD.consumed_at, OLD.consumed_by) THEN
        RAISE EXCEPTION 'a routing grant is consumed once' USING ERRCODE = '42501';
    END IF;
    IF OLD.revoked_at IS NOT NULL AND (NEW.revoked_at, NEW.revoke_reason) IS DISTINCT FROM (OLD.revoked_at, OLD.revoke_reason) THEN
        RAISE EXCEPTION 'a routing grant is revoked once' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_routing_grants_guard
    BEFORE UPDATE OR DELETE ON number_routing_grants
    FOR EACH ROW EXECUTE FUNCTION number_guard_routing_grant();
CREATE TRIGGER trg_number_routing_grants_no_truncate
    BEFORE TRUNCATE ON number_routing_grants
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_routing_grants WHERE expires_at >= now() - interval '1 day') THEN
        RAISE EXCEPTION 'routing grants are recorded; they cannot be represented before Workstream K';
    END IF;
END $$;
DROP TABLE IF EXISTS number_routing_grants;
DROP FUNCTION IF EXISTS number_guard_routing_grant();
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
