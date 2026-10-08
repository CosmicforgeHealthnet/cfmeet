//! Bridges and provider-level usage (Part E §44–§47, §79, §98, §108, §156).
//!
//! * `telephony_bridges`: media bridges anchored at the SIP / media edge — between public
//!   legs (phone-to-phone, callback relay) or between a public leg and the application
//!   RTC media of the Call (an opaque, single-use media join code the application media
//!   side presents). At most one live bridge per Call.
//! * `telephony_bridge_legs`: the members of a bridge and their join evidence.
//! * `telephony_usage_records`: exactly ONE final usage fact per Telephony leg
//!   (connected / billable duration, provider cost only when actually known, currency,
//!   the conservative estimate otherwise). Append-only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_bridges (
    id               UUID PRIMARY KEY,
    calling_call_id  UUID        NOT NULL,
    tenant_id        UUID        NOT NULL,
    application_id   UUID        NOT NULL,
    environment_id   UUID        NOT NULL,
    kind             TEXT        NOT NULL,
    status           TEXT        NOT NULL,
    media_join_code  VARCHAR(64),
    edge_bridge_ref  VARCHAR(128),
    end_reason       TEXT,
    created_at       TIMESTAMPTZ NOT NULL,
    established_at   TIMESTAMPTZ,
    ended_at         TIMESTAMPTZ,
    updated_at       TIMESTAMPTZ NOT NULL,
    version          BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT uq_telephony_bridges_join_code UNIQUE (media_join_code),
    CONSTRAINT ck_telephony_bridges_kind CHECK (kind IN ('PUBLIC_LEGS', 'APPLICATION_MEDIA')),
    CONSTRAINT ck_telephony_bridges_status CHECK (status IN ('PENDING', 'ACTIVE', 'ENDED', 'FAILED')),
    CONSTRAINT ck_telephony_bridges_join_code CHECK (
        (kind = 'APPLICATION_MEDIA') = (media_join_code IS NOT NULL)
        AND (media_join_code IS NULL OR media_join_code ~ '^cfmj[0-9a-z]{28,60}$')),
    CONSTRAINT ck_telephony_bridges_edge_ref
        CHECK (edge_bridge_ref IS NULL OR edge_bridge_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_telephony_bridges_established CHECK ((status = 'ACTIVE') <= (established_at IS NOT NULL)),
    CONSTRAINT ck_telephony_bridges_ended
        CHECK ((status IN ('ENDED', 'FAILED')) = (ended_at IS NOT NULL AND end_reason IS NOT NULL)),
    CONSTRAINT ck_telephony_bridges_end_reason CHECK (end_reason IS NULL OR end_reason IN (
        'COMPLETED', 'MEMBER_LEFT', 'UNBRIDGED', 'FAILED', 'ORPHANED', 'TIMEOUT')),
    CONSTRAINT ck_telephony_bridges_timestamps CHECK (updated_at >= created_at AND version >= 1)
);
CREATE UNIQUE INDEX uq_telephony_bridges_one_live_per_call ON telephony_bridges (calling_call_id)
    WHERE status IN ('PENDING', 'ACTIVE');
CREATE INDEX idx_telephony_bridges_status ON telephony_bridges (status, created_at);
CREATE TRIGGER trg_telephony_bridges_immutable
    BEFORE UPDATE ON telephony_bridges
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'calling_call_id', 'tenant_id', 'application_id', 'environment_id', 'kind',
        'media_join_code', 'created_at');
CREATE TRIGGER trg_telephony_bridges_set_once
    BEFORE UPDATE ON telephony_bridges
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'edge_bridge_ref', 'established_at', 'ended_at', 'end_reason');
CREATE TRIGGER trg_telephony_bridges_transition
    BEFORE UPDATE ON telephony_bridges
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_transition('status',
        'PENDING>ACTIVE', 'PENDING>FAILED', 'PENDING>ENDED', 'ACTIVE>ENDED', 'ACTIVE>FAILED');
CREATE TRIGGER trg_telephony_bridges_no_delete
    BEFORE DELETE ON telephony_bridges
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();

CREATE TABLE telephony_bridge_legs (
    id               UUID PRIMARY KEY,
    bridge_id        UUID        NOT NULL REFERENCES telephony_bridges (id) ON DELETE RESTRICT,
    member_kind      TEXT        NOT NULL,
    telephony_leg_id UUID REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    edge_channel_ref VARCHAR(128),
    status           TEXT        NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    joined_at        TIMESTAMPTZ,
    left_at          TIMESTAMPTZ,
    updated_at       TIMESTAMPTZ NOT NULL,
    CONSTRAINT uq_telephony_bridge_legs_leg UNIQUE (bridge_id, telephony_leg_id),
    CONSTRAINT ck_telephony_bridge_legs_kind CHECK (member_kind IN ('PUBLIC_LEG', 'APPLICATION_MEDIA')),
    CONSTRAINT ck_telephony_bridge_legs_member
        CHECK ((member_kind = 'PUBLIC_LEG') = (telephony_leg_id IS NOT NULL)),
    CONSTRAINT ck_telephony_bridge_legs_status CHECK (status IN ('EXPECTED', 'JOINED', 'LEFT')),
    CONSTRAINT ck_telephony_bridge_legs_joined CHECK ((status = 'EXPECTED') = (joined_at IS NULL)),
    CONSTRAINT ck_telephony_bridge_legs_left CHECK ((status = 'LEFT') = (left_at IS NOT NULL)),
    CONSTRAINT ck_telephony_bridge_legs_channel
        CHECK (edge_channel_ref IS NULL OR edge_channel_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$')
);
CREATE UNIQUE INDEX uq_telephony_bridge_legs_one_app_media ON telephony_bridge_legs (bridge_id)
    WHERE member_kind = 'APPLICATION_MEDIA';
CREATE INDEX idx_telephony_bridge_legs_leg ON telephony_bridge_legs (telephony_leg_id);
CREATE TRIGGER trg_telephony_bridge_legs_immutable
    BEFORE UPDATE ON telephony_bridge_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'bridge_id', 'member_kind', 'telephony_leg_id', 'created_at');
CREATE TRIGGER trg_telephony_bridge_legs_set_once
    BEFORE UPDATE ON telephony_bridge_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment('edge_channel_ref', 'joined_at', 'left_at');
CREATE TRIGGER trg_telephony_bridge_legs_transition
    BEFORE UPDATE ON telephony_bridge_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_transition('status',
        'EXPECTED>JOINED', 'EXPECTED>LEFT', 'JOINED>LEFT');

CREATE TABLE telephony_usage_records (
    id                    UUID PRIMARY KEY,
    telephony_leg_id      UUID         NOT NULL REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    calling_call_id       UUID         NOT NULL,
    calling_call_leg_id   UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    provider_id           VARCHAR(32)  REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    route_id              VARCHAR(64)  REFERENCES telecom_routes (id) ON DELETE RESTRICT,
    destination_country   CHAR(2),
    connected_duration_ms BIGINT       NOT NULL,
    billable_seconds      INTEGER      NOT NULL,
    provider_cost_minor   BIGINT,
    estimated_cost_minor  BIGINT,
    currency              CHAR(3),
    cost_known            BOOLEAN      NOT NULL,
    finalization_status   TEXT         NOT NULL,
    idempotency_key       VARCHAR(128) NOT NULL,
    finalized_at          TIMESTAMPTZ  NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT uq_telephony_usage_records_leg UNIQUE (telephony_leg_id),
    CONSTRAINT uq_telephony_usage_records_idempotency UNIQUE (idempotency_key),
    CONSTRAINT ck_telephony_usage_records_duration CHECK (connected_duration_ms >= 0 AND billable_seconds >= 0),
    CONSTRAINT ck_telephony_usage_records_cost CHECK (
        cost_known = (provider_cost_minor IS NOT NULL)
        AND (provider_cost_minor IS NULL OR provider_cost_minor >= 0)
        AND (estimated_cost_minor IS NULL OR estimated_cost_minor >= 0)
        AND ((provider_cost_minor IS NULL AND estimated_cost_minor IS NULL) = (currency IS NULL))
        AND (currency IS NULL OR currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_telephony_usage_records_country
        CHECK (destination_country IS NULL OR destination_country ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_telephony_usage_records_status CHECK (finalization_status IN ('FINAL')),
    CONSTRAINT ck_telephony_usage_records_key CHECK (idempotency_key ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$')
);
CREATE INDEX idx_telephony_usage_records_call ON telephony_usage_records (calling_call_id);
CREATE INDEX idx_telephony_usage_records_finalized ON telephony_usage_records (finalized_at);
CREATE TRIGGER trg_telephony_usage_records_append_only
    BEFORE UPDATE OR DELETE ON telephony_usage_records
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telephony_usage_records;
DROP TABLE IF EXISTS telephony_bridge_legs;
DROP TABLE IF EXISTS telephony_bridges;
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
