//! Operational safeguards of Calling's durable work (Part I/J/K closure of §81–§109).
//!
//! * `calls.correlation_id` — the correlation id of the request that created the Call
//!   (immutable; NULL only for Calls created before this migration). Every command and
//!   event Calling issues for the Call afterwards (worker transitions, TERMINATE / BRIDGE /
//!   ANSWER / PROMPT commands to Telephony) carries it, so one Call is traceable end to end
//!   by its call id and the original request's correlation id.
//! * Leg release in the SAME transaction as the transition (§81): a DEFERRED constraint
//!   trigger on `calls` writes one durable TERMINATE command for every live public leg
//!   (and every bound PRIVATE_NETWORK leg of an enterprise SIP dialog) when a Call enters
//!   ENDING or a terminal state. It runs at COMMIT, so it sees the legs as the transaction
//!   leaves them (a leg the same transaction already ended gets none) and it commits — or
//!   rolls back — with the transition itself: no crash window leaves a paid leg without a
//!   durable TERMINATE. One TERMINATE per leg (existing unique index), so the post-commit
//!   release path and this trigger never duplicate it.
//! * `call_telephony_commands.lease_claim` — the claim of the worker that leased a command:
//!   completion, rescheduling and the pre-send attempt count are guarded by it, so a stale
//!   worker whose lease expired can never overwrite the claim of the worker that reclaimed
//!   the command.
//! * `calling_incidents` gains TELEPHONY_COMMAND_DEAD, OUTBOX_DEAD_LETTER, WORKER_BACKLOG and
//!   CREDENTIAL_GUESSING; the last two may be platform-wide (no tenant scope). Resolution
//!   is an operator action audited in `platform_operator_audit` (INCIDENT_RESOLVED).
//! * `platform_auth_failure_windows` — bounded failed-authentication counters per presented
//!   credential key id (unknown key ids share ONE bucket, so guessing ids never grows the
//!   table); pruned with the request-rate windows.
//! * `call_usage_records`: at most ONE finalized time fact per (Call, usage type, subject)
//!   — a different idempotency key can never bill the same subject twice. Artifact facts
//!   (several recordings / messages of one Call) are per artifact and are not included.
//! * A partial index for the setup-deadline sweep over pre-connect Calls.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Call correlation id (immutable).
-- ---------------------------------------------------------------------------
ALTER TABLE calls
    ADD COLUMN correlation_id VARCHAR(37),
    ADD CONSTRAINT ck_calls_correlation_id
        CHECK (correlation_id IS NULL OR correlation_id ~ '^corr_[0-9a-f]{32}$');
CREATE TRIGGER trg_calls_correlation_immutable
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('correlation_id');

-- ---------------------------------------------------------------------------
-- Durable leg release committed with the ENDING / terminal transition.
-- ---------------------------------------------------------------------------
CREATE FUNCTION calling_enqueue_leg_release() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    leg         RECORD;
    command_id  uuid;
    correlation text := NEW.correlation_id;
BEGIN
    IF correlation IS NULL THEN
        SELECT e.correlation_id INTO correlation FROM call_events e
         WHERE e.call_id = NEW.id AND e.sequence = 1;
    END IF;
    FOR leg IN
        SELECT l.id FROM call_legs l
         WHERE l.call_id = NEW.id
           AND l.state NOT IN ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')
           AND (EXISTS (SELECT 1 FROM call_public_parties p
                         WHERE p.call_id = NEW.id AND p.call_leg_id = l.id)
                OR (l.leg_kind = 'PRIVATE_NETWORK' AND l.execution_binding_ref IS NOT NULL))
         ORDER BY l.created_at, l.id
    LOOP
        command_id := gen_random_uuid();
        INSERT INTO call_telephony_commands (id, call_id, tenant_id, application_id, environment_id,
            call_leg_id, kind, idempotency_key, payload, status, attempts, next_attempt_at,
            created_at, updated_at)
        VALUES (command_id, NEW.id, NEW.tenant_id, NEW.application_id, NEW.environment_id,
            leg.id, 'TERMINATE', 'terminate:' || leg.id::text,
            jsonb_strip_nulls(jsonb_build_object('commandId', command_id,
                'correlationId', correlation, 'reason', 'CALL_ENDED')),
            'PENDING', 0, NEW.updated_at, NEW.updated_at, NEW.updated_at)
        ON CONFLICT DO NOTHING;
    END LOOP;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER trg_calls_release_public_legs
    AFTER UPDATE OF current_state ON calls
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN (OLD.current_state IS DISTINCT FROM NEW.current_state
          AND NEW.current_state IN ('ENDING', 'ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED'))
    EXECUTE FUNCTION calling_enqueue_leg_release();

-- ---------------------------------------------------------------------------
-- Telephony command claims; backlog sampling.
-- ---------------------------------------------------------------------------
ALTER TABLE call_telephony_commands ADD COLUMN lease_claim UUID;
CREATE INDEX idx_call_telephony_commands_pending_age
    ON call_telephony_commands (created_at) WHERE status = 'PENDING';
CREATE INDEX idx_call_telephony_commands_dead
    ON call_telephony_commands (updated_at) WHERE status = 'DEAD';

-- ---------------------------------------------------------------------------
-- Pre-connect Calls (setup deadline sweep).
-- ---------------------------------------------------------------------------
CREATE INDEX idx_calls_setup_pending ON calls (requested_at)
    WHERE current_state IN ('REQUESTED', 'AUTHORIZED', 'RESOLVING', 'ROUTING', 'DIALING', 'RINGING');

-- ---------------------------------------------------------------------------
-- Incidents: worker / delivery / credential categories; platform-wide scope.
-- ---------------------------------------------------------------------------
ALTER TABLE calling_incidents
    ALTER COLUMN tenant_id DROP NOT NULL,
    ALTER COLUMN application_id DROP NOT NULL,
    ALTER COLUMN environment_id DROP NOT NULL,
    DROP CONSTRAINT ck_calling_incidents_category,
    ADD CONSTRAINT ck_calling_incidents_category CHECK (category IN (
        'STUCK_QUEUE_ENTRY', 'QUEUE_ASSIGNMENT_CONFLICT', 'IVR_ROUTING_CORRUPTION',
        'VOICEMAIL_FINALIZATION_FAILURE', 'RECORDING_ARTIFACT_FAILURE', 'FORWARDING_LOOP_REPEATED',
        'REPEATED_BLOCKED_ATTEMPTS', 'REPEATED_ROUTE_DENIALS', 'ABUSE_REPORTS_BURST',
        'TELEPHONY_COMMAND_DEAD', 'OUTBOX_DEAD_LETTER', 'WORKER_BACKLOG', 'CREDENTIAL_GUESSING')),
    DROP CONSTRAINT ck_calling_incidents_resource_type,
    ADD CONSTRAINT ck_calling_incidents_resource_type CHECK (resource_type IS NULL OR resource_type IN (
        'QUEUE_ENTRY', 'IVR_SESSION', 'VOICEMAIL_MESSAGE', 'CALL_RECORDING', 'DESTINATION', 'PRINCIPAL',
        'TELEPHONY_COMMAND', 'OUTBOX_EVENT', 'CREDENTIAL')),
    ADD CONSTRAINT ck_calling_incidents_scope CHECK (
        (tenant_id IS NULL) = (application_id IS NULL)
        AND (application_id IS NULL) = (environment_id IS NULL)
        AND (tenant_id IS NOT NULL OR category IN ('WORKER_BACKLOG', 'CREDENTIAL_GUESSING')));

-- Incident resolution is an audited operator action.
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_action;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_action CHECK (action IN (
    'TENANT_STATUS_SET', 'APPLICATION_STATUS_SET', 'ENVIRONMENT_STATUS_SET',
    'ENVIRONMENT_LIVE_TELECOM_SET', 'SERVICE_ACCOUNT_STATUS_SET',
    'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
    'SCOPE_GRANTED', 'SCOPE_REVOKED', 'LIMITS_SET', 'ENTERPRISE_CAPABILITIES_SET',
    'INBOUND_OWNER_SET', 'COMPLIANCE_ELIGIBILITY_SET', 'ABUSE_REPORT_STATUS_SET',
    'BLOCK_RULE_RELEASED', 'PRINCIPAL_STATUS_SET',
    'TENANT_CREATED', 'APPLICATION_CREATED', 'ENVIRONMENT_CREATED',
    'SERVICE_ACCOUNT_CREATED', 'PRINCIPAL_REGISTERED', 'ROUTING_POLICY_SET',
    'SPEND_POLICY_SET', 'COMPLIANCE_ELIGIBILITY_REVOKED',
    'RETENTION_HOLD_PLACED', 'RETENTION_HOLD_RELEASED', 'PRINCIPAL_TOMBSTONED',
    'WEBHOOK_KEYS_RESEALED', 'INCIDENT_RESOLVED')) NOT VALID;
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_target_kind;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_target_kind CHECK (target_kind IN (
    'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
    'BLOCK_RULE', 'ABUSE_REPORT', 'COMPLIANCE_ELIGIBILITY', 'RETENTION_HOLD', 'WEBHOOK_KEY_RING',
    'INCIDENT')) NOT VALID;

-- ---------------------------------------------------------------------------
-- Failed-authentication windows (disposable counters, like the request-rate windows).
-- ---------------------------------------------------------------------------
CREATE UNLOGGED TABLE platform_auth_failure_windows (
    subject_key  VARCHAR(32) NOT NULL,
    window_start TIMESTAMPTZ NOT NULL,
    failures     BIGINT      NOT NULL,
    CONSTRAINT pk_platform_auth_failure_windows PRIMARY KEY (subject_key, window_start),
    -- A credential key id (24 lowercase hex) or the single shared bucket of unknown ids.
    CONSTRAINT ck_platform_auth_failure_windows_subject
        CHECK (subject_key ~ '^[0-9a-f]{24}$' OR subject_key = 'unknown'),
    CONSTRAINT ck_platform_auth_failure_windows_failures CHECK (failures > 0)
);
CREATE INDEX idx_platform_auth_failure_windows_start ON platform_auth_failure_windows (window_start);

-- ---------------------------------------------------------------------------
-- One finalized time fact per (Call, usage type, subject).
-- ---------------------------------------------------------------------------
CREATE UNIQUE INDEX uq_call_usage_records_one_final_fact
    ON call_usage_records (call_id, usage_type, (COALESCE(session_id, leg_id, participant_id, call_id)))
    WHERE finalized_at IS NOT NULL
      AND usage_type IN ('CALL_CONNECTED_TIME', 'PARTICIPANT_CONNECTED_TIME',
                         'SESSION_CONNECTED_TIME', 'LEG_CONNECTED_TIME');
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS uq_call_usage_records_one_final_fact;
DROP TABLE IF EXISTS platform_auth_failure_windows;

-- The operator audit is append-only: rows written with the new codes stay; the original
-- vocabulary returns as NOT VALID (as migration 000038 does).
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_action;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_action CHECK (action IN (
    'TENANT_STATUS_SET', 'APPLICATION_STATUS_SET', 'ENVIRONMENT_STATUS_SET',
    'ENVIRONMENT_LIVE_TELECOM_SET', 'SERVICE_ACCOUNT_STATUS_SET',
    'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
    'SCOPE_GRANTED', 'SCOPE_REVOKED', 'LIMITS_SET', 'ENTERPRISE_CAPABILITIES_SET',
    'INBOUND_OWNER_SET', 'COMPLIANCE_ELIGIBILITY_SET', 'ABUSE_REPORT_STATUS_SET',
    'BLOCK_RULE_RELEASED', 'PRINCIPAL_STATUS_SET',
    'TENANT_CREATED', 'APPLICATION_CREATED', 'ENVIRONMENT_CREATED',
    'SERVICE_ACCOUNT_CREATED', 'PRINCIPAL_REGISTERED', 'ROUTING_POLICY_SET',
    'SPEND_POLICY_SET', 'COMPLIANCE_ELIGIBILITY_REVOKED',
    'RETENTION_HOLD_PLACED', 'RETENTION_HOLD_RELEASED', 'PRINCIPAL_TOMBSTONED',
    'WEBHOOK_KEYS_RESEALED')) NOT VALID;
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_target_kind;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_target_kind CHECK (target_kind IN (
    'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
    'BLOCK_RULE', 'ABUSE_REPORT', 'COMPLIANCE_ELIGIBILITY', 'RETENTION_HOLD', 'WEBHOOK_KEY_RING')) NOT VALID;

-- Incidents of the new vocabulary are removed (deletes are allowed only to the retention
-- worker; this rollback acts as it).
SELECT set_config('calling.retention_worker', 'on', true);
DELETE FROM calling_incidents WHERE category IN (
    'TELEPHONY_COMMAND_DEAD', 'OUTBOX_DEAD_LETTER', 'WORKER_BACKLOG', 'CREDENTIAL_GUESSING')
    OR resource_type IN ('TELEPHONY_COMMAND', 'OUTBOX_EVENT', 'CREDENTIAL')
    OR tenant_id IS NULL;
SELECT set_config('calling.retention_worker', 'off', true);
ALTER TABLE calling_incidents
    DROP CONSTRAINT ck_calling_incidents_scope,
    DROP CONSTRAINT ck_calling_incidents_category,
    ADD CONSTRAINT ck_calling_incidents_category CHECK (category IN (
        'STUCK_QUEUE_ENTRY', 'QUEUE_ASSIGNMENT_CONFLICT', 'IVR_ROUTING_CORRUPTION',
        'VOICEMAIL_FINALIZATION_FAILURE', 'RECORDING_ARTIFACT_FAILURE', 'FORWARDING_LOOP_REPEATED',
        'REPEATED_BLOCKED_ATTEMPTS', 'REPEATED_ROUTE_DENIALS', 'ABUSE_REPORTS_BURST')),
    DROP CONSTRAINT ck_calling_incidents_resource_type,
    ADD CONSTRAINT ck_calling_incidents_resource_type CHECK (resource_type IS NULL OR resource_type IN (
        'QUEUE_ENTRY', 'IVR_SESSION', 'VOICEMAIL_MESSAGE', 'CALL_RECORDING', 'DESTINATION', 'PRINCIPAL')),
    ALTER COLUMN tenant_id SET NOT NULL,
    ALTER COLUMN application_id SET NOT NULL,
    ALTER COLUMN environment_id SET NOT NULL;

DROP INDEX IF EXISTS idx_calls_setup_pending;
DROP INDEX IF EXISTS idx_call_telephony_commands_dead;
DROP INDEX IF EXISTS idx_call_telephony_commands_pending_age;
ALTER TABLE call_telephony_commands DROP COLUMN IF EXISTS lease_claim;

DROP TRIGGER IF EXISTS trg_calls_release_public_legs ON calls;
DROP FUNCTION IF EXISTS calling_enqueue_leg_release();

DROP TRIGGER IF EXISTS trg_calls_correlation_immutable ON calls;
ALTER TABLE calls
    DROP CONSTRAINT IF EXISTS ck_calls_correlation_id,
    DROP COLUMN IF EXISTS correlation_id;
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
