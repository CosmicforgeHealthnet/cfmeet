//! Admin bridge owner events, the HQ feed and feature switch-on requests (ADR 0068,
//! design §10, §11 and §13, phases A7 and A8).
//!
//! * `admin_event_inbox` — one row per owner event received, keyed by the authenticated
//!   producer and the producer's event id (dedupe): APPENDED to the feed, STALE (a version
//!   not newer than the subject's last one) or QUARANTINED (a type the bridge does not
//!   accept from that producer). Append-only.
//! * `admin_hq_outbox` — the append-only feed HQ pulls, in increasing `sequence`. Appends are
//!   serialized by the bridge (advisory lock), so sequences commit in order. Ids, states,
//!   versions and times only: never personal data.
//! * `admin_hq_consumer_cursors` — one acknowledged sequence per HQ consumer; it never moves
//!   backwards and is never deleted.
//! * `admin_activation_requests` — every feature switch-on request HQ made (request id = the
//!   HQ operation id): the release workflow operation, the commit, who asked, who approved,
//!   and the dispatch state REQUESTED → DISPATCHED | FAILED. The workflow run id is linked
//!   once. Never deleted; the request itself never changes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Advisory lock key serializing feed appends (sequence order = commit order).
pub const EVENTS_LOCK_KEY: i64 = 0x0adb_1d6e_a0d1_7002;

const UP: &str = r#"
-- ---- Owner events ---------------------------------------------------------------------
CREATE TABLE admin_event_inbox (
    producer      VARCHAR(32)  NOT NULL,
    event_id      UUID         NOT NULL,
    event_type    VARCHAR(96)  NOT NULL,
    disposition   VARCHAR(16)  NOT NULL,
    received_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_admin_event_inbox PRIMARY KEY (producer, event_id),
    CONSTRAINT ck_admin_event_inbox_producer CHECK (producer ~ '^[a-z][a-z0-9-]{0,31}$'),
    CONSTRAINT ck_admin_event_inbox_type CHECK (event_type ~ '^[a-z][a-z0-9._-]{0,95}$'),
    CONSTRAINT ck_admin_event_inbox_disposition CHECK (disposition IN ('APPENDED', 'STALE', 'QUARANTINED'))
);
CREATE TRIGGER tr_admin_event_inbox_update BEFORE UPDATE OR DELETE ON admin_event_inbox
    FOR EACH ROW EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_event_inbox_truncate BEFORE TRUNCATE ON admin_event_inbox
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

CREATE TABLE admin_hq_outbox (
    sequence      BIGINT       GENERATED ALWAYS AS IDENTITY,
    producer      VARCHAR(32)  NOT NULL,
    event_id      UUID         NOT NULL,
    event_type    VARCHAR(96)  NOT NULL,
    subject_kind  VARCHAR(48)  NOT NULL,
    subject_id    VARCHAR(128) NOT NULL,
    state         VARCHAR(64)  NOT NULL,
    version       BIGINT       NOT NULL,
    occurred_at   TIMESTAMPTZ  NOT NULL,
    appended_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_admin_hq_outbox PRIMARY KEY (sequence),
    CONSTRAINT uq_admin_hq_outbox_event UNIQUE (producer, event_id),
    CONSTRAINT fk_admin_hq_outbox_inbox FOREIGN KEY (producer, event_id)
        REFERENCES admin_event_inbox (producer, event_id),
    CONSTRAINT ck_admin_hq_outbox_type CHECK (event_type IN (
        'verification.case.changed.v1', 'abuse.report.changed.v1', 'partner.workspace.changed.v1',
        'app.review.changed.v1', 'incident.changed.v1', 'pricing.version.changed.v1')),
    CONSTRAINT ck_admin_hq_outbox_subject_kind CHECK (subject_kind ~ '^[a-z][a-z0-9_]{0,47}$'),
    CONSTRAINT ck_admin_hq_outbox_subject_id CHECK (subject_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_admin_hq_outbox_state CHECK (state ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_admin_hq_outbox_version CHECK (version >= 1)
);
CREATE INDEX ix_admin_hq_outbox_subject ON admin_hq_outbox (producer, subject_kind, subject_id, version);
CREATE INDEX ix_admin_hq_outbox_appended ON admin_hq_outbox (appended_at, sequence);
CREATE TRIGGER tr_admin_hq_outbox_update BEFORE UPDATE OR DELETE ON admin_hq_outbox
    FOR EACH ROW EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_hq_outbox_truncate BEFORE TRUNCATE ON admin_hq_outbox
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

CREATE TABLE admin_hq_consumer_cursors (
    consumer               VARCHAR(64)  NOT NULL,
    acknowledged_sequence  BIGINT       NOT NULL,
    updated_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_admin_hq_consumer_cursors PRIMARY KEY (consumer),
    CONSTRAINT ck_admin_hq_consumer_cursors_consumer CHECK (consumer ~ '^[a-z][a-z0-9._-]{0,63}$'),
    CONSTRAINT ck_admin_hq_consumer_cursors_sequence CHECK (acknowledged_sequence >= 0)
);

CREATE FUNCTION admin_hq_consumer_cursors_forward() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'admin_hq_consumer_cursors: rows are never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.consumer IS DISTINCT FROM OLD.consumer OR NEW.acknowledged_sequence < OLD.acknowledged_sequence THEN
        RAISE EXCEPTION 'admin_hq_consumer_cursors: an acknowledgement never moves backwards'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_admin_hq_consumer_cursors_forward BEFORE UPDATE OR DELETE ON admin_hq_consumer_cursors
    FOR EACH ROW EXECUTE FUNCTION admin_hq_consumer_cursors_forward();
CREATE TRIGGER tr_admin_hq_consumer_cursors_truncate BEFORE TRUNCATE ON admin_hq_consumer_cursors
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

-- ---- Feature switch-on requests ---------------------------------------------------------
CREATE TABLE admin_activation_requests (
    request_id     UUID         NOT NULL,
    operation      VARCHAR(64)  NOT NULL,
    release_sha    VARCHAR(40)  NOT NULL,
    confirm        VARCHAR(128),
    requested_by   VARCHAR(160) NOT NULL,
    approver       VARCHAR(160) NOT NULL,
    approval_id    VARCHAR(128) NOT NULL,
    state          VARCHAR(16)  NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL,
    dispatched_at  TIMESTAMPTZ,
    run_id         BIGINT,
    CONSTRAINT pk_admin_activation_requests PRIMARY KEY (request_id),
    CONSTRAINT ck_admin_activation_requests_operation CHECK (operation ~ '^[a-z][a-z0-9-]{0,63}$'),
    CONSTRAINT ck_admin_activation_requests_sha CHECK (release_sha ~ '^[0-9a-f]{40}$'),
    CONSTRAINT ck_admin_activation_requests_confirm CHECK (confirm IS NULL OR confirm ~ '^[a-z0-9][a-z0-9=,._:-]{0,127}$'),
    CONSTRAINT ck_admin_activation_requests_people CHECK (
        requested_by ~ '^hq:[A-Za-z0-9._:@-]{1,128}$' AND approver ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'
        AND approver <> requested_by),
    CONSTRAINT ck_admin_activation_requests_state CHECK (state IN ('REQUESTED', 'DISPATCHED', 'FAILED')),
    CONSTRAINT ck_admin_activation_requests_dispatched CHECK ((state = 'DISPATCHED') = (dispatched_at IS NOT NULL)),
    CONSTRAINT ck_admin_activation_requests_run CHECK (run_id IS NULL OR (run_id >= 1 AND state = 'DISPATCHED'))
);
CREATE UNIQUE INDEX uq_admin_activation_requests_run ON admin_activation_requests (run_id)
    WHERE run_id IS NOT NULL;
CREATE INDEX ix_admin_activation_requests_created ON admin_activation_requests (created_at, request_id);

CREATE FUNCTION admin_activation_requests_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'admin_activation_requests: rows are never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF (NEW.request_id, NEW.operation, NEW.release_sha, NEW.confirm, NEW.requested_by, NEW.approver,
        NEW.approval_id, NEW.created_at)
       IS DISTINCT FROM
       (OLD.request_id, OLD.operation, OLD.release_sha, OLD.confirm, OLD.requested_by, OLD.approver,
        OLD.approval_id, OLD.created_at)
       OR (OLD.dispatched_at IS NOT NULL AND NEW.dispatched_at IS DISTINCT FROM OLD.dispatched_at)
       OR (OLD.run_id IS NOT NULL AND NEW.run_id IS DISTINCT FROM OLD.run_id) THEN
        RAISE EXCEPTION 'admin_activation_requests: only the dispatch state and the run may be recorded'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.state <> OLD.state AND NOT (OLD.state = 'REQUESTED' AND NEW.state IN ('DISPATCHED', 'FAILED')) THEN
        RAISE EXCEPTION 'admin_activation_requests: % cannot become %', OLD.state, NEW.state
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_admin_activation_requests_guard BEFORE UPDATE OR DELETE ON admin_activation_requests
    FOR EACH ROW EXECUTE FUNCTION admin_activation_requests_guard();
CREATE TRIGGER tr_admin_activation_requests_truncate BEFORE TRUNCATE ON admin_activation_requests
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS admin_activation_requests;
DROP FUNCTION IF EXISTS admin_activation_requests_guard();
DROP TABLE IF EXISTS admin_hq_consumer_cursors;
DROP FUNCTION IF EXISTS admin_hq_consumer_cursors_forward();
DROP TABLE IF EXISTS admin_hq_outbox;
DROP TABLE IF EXISTS admin_event_inbox;
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
