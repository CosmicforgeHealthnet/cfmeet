//! Admin bridge privacy requests (ADR 0068, design §6.14, phase A7).
//!
//! One data-subject request tracked across every CF Meet service that holds the person's
//! data. The bridge stores only the request's kind, the opaque CF Meet account id it is
//! about, and each service's reported outcome: never the person's data itself.
//!
//! * `admin_privacy_requests` — kind ACCESS / ERASURE / CORRECTION / RESTRICTION, state
//!   OPEN until every service has an outcome, then COMPLETE; `version` increases on every
//!   change (the commands' expected version). Never deleted.
//! * `admin_privacy_request_services` — one row per service: PENDING, DONE, NOT_HELD or
//!   LEGAL_HOLD, who recorded it and when. A recorded outcome never changes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE admin_privacy_requests (
    request_id     UUID         NOT NULL,
    kind           VARCHAR(16)  NOT NULL,
    subject_ref    UUID         NOT NULL,
    state          VARCHAR(16)  NOT NULL,
    created_by     VARCHAR(160) NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL,
    updated_at     TIMESTAMPTZ  NOT NULL,
    completed_at   TIMESTAMPTZ,
    version        BIGINT       NOT NULL,
    CONSTRAINT pk_admin_privacy_requests PRIMARY KEY (request_id),
    CONSTRAINT ck_admin_privacy_requests_kind CHECK (kind IN ('ACCESS', 'ERASURE', 'CORRECTION', 'RESTRICTION')),
    CONSTRAINT ck_admin_privacy_requests_state CHECK (state IN ('OPEN', 'COMPLETE')),
    CONSTRAINT ck_admin_privacy_requests_complete CHECK ((state = 'COMPLETE') = (completed_at IS NOT NULL)),
    CONSTRAINT ck_admin_privacy_requests_version CHECK (version >= 1),
    CONSTRAINT ck_admin_privacy_requests_actor CHECK (created_by ~ '^hq:[A-Za-z0-9._:@-]{1,128}$')
);
CREATE INDEX ix_admin_privacy_requests_created ON admin_privacy_requests (created_at, request_id);

CREATE TABLE admin_privacy_request_services (
    request_id     UUID         NOT NULL,
    service        VARCHAR(32)  NOT NULL,
    outcome        VARCHAR(16)  NOT NULL,
    recorded_by    VARCHAR(160),
    recorded_at    TIMESTAMPTZ,
    CONSTRAINT pk_admin_privacy_request_services PRIMARY KEY (request_id, service),
    CONSTRAINT fk_admin_privacy_request_services_request FOREIGN KEY (request_id)
        REFERENCES admin_privacy_requests (request_id),
    CONSTRAINT ck_admin_privacy_request_services_outcome CHECK (outcome IN ('PENDING', 'DONE', 'NOT_HELD', 'LEGAL_HOLD')),
    CONSTRAINT ck_admin_privacy_request_services_recorded CHECK
        ((outcome = 'PENDING') = (recorded_at IS NULL) AND (recorded_at IS NULL) = (recorded_by IS NULL))
);

CREATE FUNCTION admin_privacy_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION '%: rows are never deleted', TG_TABLE_NAME USING ERRCODE = 'check_violation';
    END IF;
    IF TG_TABLE_NAME = 'admin_privacy_request_services' AND OLD.outcome <> 'PENDING' THEN
        RAISE EXCEPTION 'admin_privacy_request_services: a recorded outcome never changes'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_TABLE_NAME = 'admin_privacy_requests' AND (NEW.request_id, NEW.kind, NEW.subject_ref, NEW.created_by, NEW.created_at)
        IS DISTINCT FROM (OLD.request_id, OLD.kind, OLD.subject_ref, OLD.created_by, OLD.created_at) THEN
        RAISE EXCEPTION 'admin_privacy_requests: only the state may change' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_admin_privacy_requests_guard BEFORE UPDATE OR DELETE ON admin_privacy_requests
    FOR EACH ROW EXECUTE FUNCTION admin_privacy_guard();
CREATE TRIGGER tr_admin_privacy_request_services_guard BEFORE UPDATE OR DELETE ON admin_privacy_request_services
    FOR EACH ROW EXECUTE FUNCTION admin_privacy_guard();
CREATE TRIGGER tr_admin_privacy_requests_truncate BEFORE TRUNCATE ON admin_privacy_requests
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_privacy_request_services_truncate BEFORE TRUNCATE ON admin_privacy_request_services
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS admin_privacy_request_services;
DROP TABLE IF EXISTS admin_privacy_requests;
DROP FUNCTION IF EXISTS admin_privacy_guard();
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
