//! Incidents and reconciliation evidence (Part E §50, §137–§138).
//!
//! * `telephony_incidents`: ambiguous or suspicious conditions the reconciler (or event
//!   ingestion) refuses to guess about. Deduplicated while open by a stable key; carries
//!   severity, category, safe provider / route references, CF Meet Call / CallLeg /
//!   Telephony leg references, first / last seen, the current state, occurrence and
//!   escalation counts and a bounded RESTRICTED evidence object. Never a telephone
//!   number.
//! * `telephony_reconciliation_runs`: each repair decision the reconciler took (what it
//!   saw, what it did), so every automatic repair is auditable and exactly-once.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_incidents (
    id                  UUID PRIMARY KEY,
    dedupe_key          VARCHAR(160) NOT NULL,
    severity            TEXT         NOT NULL,
    category            TEXT         NOT NULL,
    status              TEXT         NOT NULL,
    provider_id         VARCHAR(32),
    route_id            VARCHAR(64),
    calling_call_id     UUID,
    calling_call_leg_id UUID,
    telephony_leg_id    UUID,
    current_state       VARCHAR(64),
    occurrences         INTEGER      NOT NULL DEFAULT 1,
    escalation_count    INTEGER      NOT NULL DEFAULT 0,
    restricted_evidence JSONB        NOT NULL,
    first_seen_at       TIMESTAMPTZ  NOT NULL,
    last_seen_at        TIMESTAMPTZ  NOT NULL,
    resolved_at         TIMESTAMPTZ,
    CONSTRAINT ck_telephony_incidents_severity CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED',
        'ABUSE_SIGNAL')),
    CONSTRAINT ck_telephony_incidents_status CHECK (status IN ('OPEN', 'RESOLVED')),
    CONSTRAINT ck_telephony_incidents_resolved CHECK ((status = 'RESOLVED') = (resolved_at IS NOT NULL)),
    CONSTRAINT ck_telephony_incidents_counts CHECK (occurrences >= 1 AND escalation_count >= 0),
    CONSTRAINT ck_telephony_incidents_evidence CHECK (
        jsonb_typeof(restricted_evidence) = 'object' AND octet_length(restricted_evidence::text) <= 4096),
    CONSTRAINT ck_telephony_incidents_seen CHECK (last_seen_at >= first_seen_at)
);
CREATE UNIQUE INDEX uq_telephony_incidents_open ON telephony_incidents (dedupe_key) WHERE status = 'OPEN';
CREATE INDEX idx_telephony_incidents_status_severity ON telephony_incidents (status, severity, last_seen_at);
CREATE INDEX idx_telephony_incidents_leg ON telephony_incidents (telephony_leg_id);
CREATE TRIGGER trg_telephony_incidents_immutable
    BEFORE UPDATE ON telephony_incidents
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'dedupe_key', 'category', 'provider_id', 'route_id', 'calling_call_id',
        'calling_call_leg_id', 'telephony_leg_id', 'first_seen_at');
CREATE TRIGGER trg_telephony_incidents_transition
    BEFORE UPDATE ON telephony_incidents
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_transition('status', 'OPEN>RESOLVED');

CREATE TABLE telephony_reconciliation_runs (
    id               UUID PRIMARY KEY,
    telephony_leg_id UUID         REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    command_id       UUID         REFERENCES telephony_commands (id) ON DELETE RESTRICT,
    bridge_id        UUID,
    case_code        TEXT         NOT NULL,
    observed         TEXT         NOT NULL,
    action           TEXT         NOT NULL,
    worker_id        VARCHAR(64)  NOT NULL,
    decided_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT ck_telephony_reconciliation_runs_case CHECK (case_code IN (
        'RINGING_TIMEOUT', 'CONNECTED_PROVIDER_ENDED', 'PROVIDER_ACTIVE_NO_BINDING', 'ORPHAN_BRIDGE_LEG',
        'UNKNOWN_ORIGINATION', 'UNKNOWN_TERMINATION', 'EVENT_GAP', 'MISSING_USAGE',
        'TERMINAL_CALL_ACTIVE_LEG', 'DURATION_LIMIT', 'STALE_LEASE')),
    CONSTRAINT ck_telephony_reconciliation_runs_observed CHECK (observed ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_telephony_reconciliation_runs_action CHECK (action IN (
        'MARKED_ENDED', 'MARKED_FAILED', 'MARKED_ACTIVE', 'TERMINATED_ORPHAN', 'FINALIZED_USAGE',
        'INCIDENT_OPENED', 'RESOLVED_ABSENT', 'RESOLVED_ACCEPTED', 'NO_CHANGE', 'RETRY_SCHEDULED')),
    CONSTRAINT ck_telephony_reconciliation_runs_target
        CHECK (telephony_leg_id IS NOT NULL OR command_id IS NOT NULL OR bridge_id IS NOT NULL)
);
CREATE INDEX idx_telephony_reconciliation_runs_leg ON telephony_reconciliation_runs (telephony_leg_id);
CREATE TRIGGER trg_telephony_reconciliation_runs_append_only
    BEFORE UPDATE OR DELETE ON telephony_reconciliation_runs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telephony_reconciliation_runs;
DROP TABLE IF EXISTS telephony_incidents;
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
