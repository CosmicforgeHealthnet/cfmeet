//! Calling retention, deletion and minimization (Part H §80, ADR 0030).
//!
//! * `calling_retention_holds`: an operator hold that keeps a resource, a scope's data
//!   class, or (platform-wide, `*` only) everything out of every retention action until
//!   it is RELEASED (never deleted). A scope hold names tenant, application AND
//!   environment, so a hold in one scope never protects another.
//!   `calling_retention_held(class, resource, tenant, application, environment)` is the
//!   single matching rule the workers use.
//! * `calling_retention_audit`: append-only evidence of every retention batch and every
//!   tombstone (class, action, count, policy, actor, outcome) — never the content.
//! * Retention worker flag: the guard triggers of the purgeable evidence tables
//!   (webhook events / deliveries / attempts, Calling incidents, media-provider and
//!   telephony provider events, the operator audit) accept a DELETE only inside a
//!   transaction that ran `SET LOCAL calling.retention_worker = 'on'`; UPDATE (and
//!   TRUNCATE) stay refused for everyone. Calls, events, usage, spend and the other
//!   evidence tables keep their unconditional guards: no flag deletes them.
//! * Minimization of terminal Calls: `call_participants.display_minimized_at` (set once,
//!   only under the worker flag, and the display name is NULL from then on — it can never
//!   reappear) and `enterprise_call_contexts.entry_label` (may only be replaced by the
//!   entry extension code, under the flag, and never changes again).
//! * Principal tombstone (§80.42): `platform_principals.tombstoned_at` (set once, only
//!   with status DEACTIVATED — a tombstoned principal can never be reactivated); the
//!   immutable `external_reference` may change exactly once, in the tombstoning update,
//!   to the non-identifying tombstone value derived from the principal id.
//! * The operator audit vocabulary gains `RETENTION_HOLD_PLACED`,
//!   `RETENTION_HOLD_RELEASED`, `PRINCIPAL_TOMBSTONED`, `WEBHOOK_KEYS_RESEALED` and the
//!   target kinds `RETENTION_HOLD` / `WEBHOOK_KEY_RING`. DOWN restores the 000038 lists
//!   `NOT VALID` (audit rows are append-only; a rollback must not fail on them); UP adds
//!   its lists `NOT VALID` as well (new rows are checked; a re-apply after a rollback
//!   never fails on rows a later vocabulary wrote).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Operator audit vocabulary.
-- ---------------------------------------------------------------------------
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

-- ---------------------------------------------------------------------------
-- Retention worker flag: DELETE only inside a retention transaction.
-- ---------------------------------------------------------------------------
CREATE FUNCTION calling_forbid_mutation_outside_retention() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND COALESCE(current_setting('calling.retention_worker', true), '') = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'calling invariant violated: % rows are immutable evidence (% refused outside the retention worker)',
        TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

DROP TRIGGER trg_platform_operator_audit_no_update ON platform_operator_audit;
CREATE TRIGGER trg_platform_operator_audit_no_update
    BEFORE UPDATE OR DELETE ON platform_operator_audit
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();

DROP TRIGGER trg_webhook_events_immutable ON webhook_events;
CREATE TRIGGER trg_webhook_events_immutable
    BEFORE UPDATE OR DELETE ON webhook_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();
CREATE TRIGGER trg_webhook_events_no_truncate
    BEFORE TRUNCATE ON webhook_events
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

DROP TRIGGER trg_webhook_deliveries_no_delete ON webhook_deliveries;
CREATE TRIGGER trg_webhook_deliveries_no_delete
    BEFORE DELETE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();
CREATE TRIGGER trg_webhook_deliveries_no_truncate
    BEFORE TRUNCATE ON webhook_deliveries
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

DROP TRIGGER trg_webhook_delivery_attempts_immutable ON webhook_delivery_attempts;
CREATE TRIGGER trg_webhook_delivery_attempts_immutable
    BEFORE UPDATE OR DELETE ON webhook_delivery_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();
CREATE TRIGGER trg_webhook_delivery_attempts_no_truncate
    BEFORE TRUNCATE ON webhook_delivery_attempts
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

DROP TRIGGER trg_call_telephony_events_append_only ON call_telephony_events;
CREATE TRIGGER trg_call_telephony_events_append_only
    BEFORE UPDATE OR DELETE ON call_telephony_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();
CREATE TRIGGER trg_call_telephony_events_no_truncate
    BEFORE TRUNCATE ON call_telephony_events
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

CREATE TRIGGER trg_calling_incidents_retention_worker_only
    BEFORE DELETE ON calling_incidents
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();
CREATE TRIGGER trg_calling_incidents_no_truncate
    BEFORE TRUNCATE ON calling_incidents
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

CREATE TRIGGER trg_call_media_provider_events_retention_worker_only
    BEFORE DELETE ON call_media_provider_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation_outside_retention();
CREATE TRIGGER trg_call_media_provider_events_no_truncate
    BEFORE TRUNCATE ON call_media_provider_events
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Retention holds.
-- ---------------------------------------------------------------------------
CREATE TABLE calling_retention_holds (
    id             UUID         NOT NULL,
    data_class     VARCHAR(64)  NOT NULL,
    resource_id    UUID,
    tenant_id      UUID,
    application_id UUID,
    environment_id UUID,
    reason_code    TEXT         NOT NULL,
    created_by     VARCHAR(100) NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL,
    released_at    TIMESTAMPTZ,
    CONSTRAINT pk_calling_retention_holds PRIMARY KEY (id),
    CONSTRAINT ck_calling_retention_holds_class CHECK (data_class IN ('*',
        'CALL_RECORD', 'CALL_EVENT', 'CALL_USAGE', 'SPEND_EVIDENCE', 'WEBHOOK_EVENT_HISTORY',
        'WEBHOOK_DELIVERY_HISTORY', 'CALLING_INCIDENT', 'MEDIA_PROVIDER_EVIDENCE',
        'OPERATOR_AUDIT', 'ADMIN_AUDIT', 'RECORDING_ACCESS_AUDIT', 'RECORDING_ARTIFACT',
        'VOICEMAIL_ARTIFACT', 'PRINCIPAL')),
    CONSTRAINT ck_calling_retention_holds_reason CHECK (reason_code IN (
        'POLICY_REVIEW', 'DISPUTE', 'SECURITY_INVESTIGATION', 'OTHER')),
    CONSTRAINT ck_calling_retention_holds_created_by CHECK (created_by ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$'),
    -- A scope hold names the whole scope; only a platform-wide hold over every class
    -- (`*`, no resource) may leave it out.
    CONSTRAINT ck_calling_retention_holds_scope CHECK (
        (tenant_id IS NOT NULL AND application_id IS NOT NULL AND environment_id IS NOT NULL)
        OR (tenant_id IS NULL AND application_id IS NULL AND environment_id IS NULL
            AND data_class = '*' AND resource_id IS NULL)),
    CONSTRAINT ck_calling_retention_holds_released CHECK (released_at IS NULL OR released_at >= created_at),
    CONSTRAINT fk_calling_retention_holds_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT
);
CREATE INDEX idx_calling_retention_holds_active ON calling_retention_holds (data_class) WHERE released_at IS NULL;
CREATE TRIGGER trg_calling_retention_holds_immutable
    BEFORE UPDATE ON calling_retention_holds
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'data_class', 'resource_id', 'tenant_id', 'application_id', 'environment_id',
        'reason_code', 'created_by', 'created_at');
CREATE TRIGGER trg_calling_retention_holds_released_once
    BEFORE UPDATE ON calling_retention_holds
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('released_at');
CREATE TRIGGER trg_calling_retention_holds_no_delete
    BEFORE DELETE ON calling_retention_holds
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
CREATE TRIGGER trg_calling_retention_holds_no_truncate
    BEFORE TRUNCATE ON calling_retention_holds
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

-- Whether an active hold covers (class, resource, scope). Class: the hold's class or `*`.
-- Resource: none named, or the same one. Scope: platform-wide, or ALL THREE ids equal
-- (a NULL row scope matches platform-wide holds only).
CREATE FUNCTION calling_retention_held(class text, resource uuid, tenant uuid, app uuid, env uuid)
RETURNS boolean LANGUAGE sql STABLE AS $$
    SELECT EXISTS (
        SELECT 1 FROM calling_retention_holds h
        WHERE h.released_at IS NULL
          AND (h.data_class = '*' OR h.data_class = class)
          AND (h.resource_id IS NULL OR h.resource_id = resource)
          AND ((h.tenant_id IS NULL AND h.application_id IS NULL AND h.environment_id IS NULL)
               OR (h.tenant_id = tenant AND h.application_id = app AND h.environment_id = env)))
$$;

-- ---------------------------------------------------------------------------
-- Retention audit (append-only, never content).
-- ---------------------------------------------------------------------------
CREATE TABLE calling_retention_audit (
    id           UUID         NOT NULL,
    data_class   VARCHAR(64)  NOT NULL,
    action       TEXT         NOT NULL,
    resource_ref UUID,
    batch_count  INTEGER      NOT NULL,
    policy       VARCHAR(128) NOT NULL,
    actor        VARCHAR(100) NOT NULL,
    outcome      TEXT         NOT NULL,
    occurred_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_calling_retention_audit PRIMARY KEY (id),
    CONSTRAINT ck_calling_retention_audit_class CHECK (data_class IN (
        'CALL_RECORD', 'CALL_EVENT', 'CALL_USAGE', 'SPEND_EVIDENCE', 'WEBHOOK_EVENT_HISTORY',
        'WEBHOOK_DELIVERY_HISTORY', 'CALLING_INCIDENT', 'MEDIA_PROVIDER_EVIDENCE',
        'OPERATOR_AUDIT', 'ADMIN_AUDIT', 'RECORDING_ACCESS_AUDIT', 'RECORDING_ARTIFACT',
        'VOICEMAIL_ARTIFACT', 'PRINCIPAL')),
    CONSTRAINT ck_calling_retention_audit_action CHECK (action IN ('DELETE', 'MINIMIZE')),
    CONSTRAINT ck_calling_retention_audit_count CHECK (batch_count >= 0),
    CONSTRAINT ck_calling_retention_audit_policy CHECK (policy ~ '^[A-Z][A-Z0-9_]{0,63}(=[0-9]{1,10})?$'),
    CONSTRAINT ck_calling_retention_audit_actor CHECK (actor ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$'),
    CONSTRAINT ck_calling_retention_audit_outcome CHECK (outcome IN (
        'DELETED', 'MINIMIZED', 'SKIPPED_HOLD', 'FAILED'))
);
CREATE INDEX idx_calling_retention_audit_time ON calling_retention_audit (occurred_at);
CREATE INDEX idx_calling_retention_audit_class ON calling_retention_audit (data_class, outcome, occurred_at DESC);
CREATE TRIGGER trg_calling_retention_audit_append_only
    BEFORE UPDATE OR DELETE ON calling_retention_audit
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
CREATE TRIGGER trg_calling_retention_audit_no_truncate
    BEFORE TRUNCATE ON calling_retention_audit
    FOR EACH STATEMENT EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Minimization of terminal Calls' display metadata.
-- ---------------------------------------------------------------------------
ALTER TABLE call_participants
    ADD COLUMN display_minimized_at TIMESTAMPTZ,
    ADD CONSTRAINT ck_call_participants_display_minimized CHECK (
        display_minimized_at IS NULL OR display_name IS NULL);
CREATE TRIGGER trg_call_participants_display_minimized_once
    BEFORE UPDATE ON call_participants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('display_minimized_at');

CREATE FUNCTION calling_guard_display_minimization() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.display_minimized_at IS DISTINCT FROM OLD.display_minimized_at
       AND current_setting('calling.retention_worker', true) IS DISTINCT FROM 'on' THEN
        RAISE EXCEPTION 'calling invariant violated: %.display_minimized_at is set by the retention worker only', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_participants_display_minimization
    BEFORE UPDATE ON call_participants
    FOR EACH ROW EXECUTE FUNCTION calling_guard_display_minimization();
CREATE INDEX idx_call_participants_display_unminimized ON call_participants (call_id)
    WHERE display_minimized_at IS NULL;

-- The entry label of an enterprise Call (a copy of the extension's display label) may be
-- replaced by the entry extension code once, by the retention worker, and never again.
DROP TRIGGER trg_enterprise_call_contexts_immutable ON enterprise_call_contexts;
CREATE TRIGGER trg_enterprise_call_contexts_immutable
    BEFORE UPDATE ON enterprise_call_contexts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'call_id', 'tenant_id', 'application_id', 'environment_id', 'entry_extension_id',
        'entry_extension_number', 'policy_id', 'policy_version', 'max_hops', 'created_at');
CREATE FUNCTION calling_guard_entry_label_minimization() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.entry_label IS DISTINCT FROM OLD.entry_label AND NOT (
        OLD.entry_label IS DISTINCT FROM OLD.entry_extension_number
        AND NEW.entry_label = NEW.entry_extension_number
        AND COALESCE(current_setting('calling.retention_worker', true), '') = 'on') THEN
        RAISE EXCEPTION 'calling invariant violated: %.entry_label is immutable (minimized by the retention worker only)', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_enterprise_call_contexts_entry_label
    BEFORE UPDATE ON enterprise_call_contexts
    FOR EACH ROW EXECUTE FUNCTION calling_guard_entry_label_minimization();
CREATE INDEX idx_enterprise_call_contexts_label_unminimized ON enterprise_call_contexts (call_id)
    WHERE entry_label <> entry_extension_number;

-- ---------------------------------------------------------------------------
-- Principal tombstone.
-- ---------------------------------------------------------------------------
ALTER TABLE platform_principals
    ADD COLUMN tombstoned_at TIMESTAMPTZ,
    ADD CONSTRAINT ck_platform_principals_tombstoned CHECK (
        tombstoned_at IS NULL OR status = 'DEACTIVATED');
CREATE TRIGGER trg_platform_principals_tombstoned_once
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('tombstoned_at');
DROP TRIGGER trg_platform_principals_immutable ON platform_principals;
CREATE TRIGGER trg_platform_principals_immutable
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'kind',
        'linked_service_account_id', 'created_at');
CREATE FUNCTION calling_guard_principal_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.external_reference IS DISTINCT FROM OLD.external_reference AND NOT (
        OLD.tombstoned_at IS NULL AND NEW.tombstoned_at IS NOT NULL
        AND NEW.external_reference = CASE WHEN NEW.kind = 'TELEPHONE_ENDPOINT' THEN NEW.id::text
                                          ELSE 'tombstoned:' || NEW.id::text END) THEN
        RAISE EXCEPTION 'platform invariant violated: %.external_reference is immutable (replaced once by a tombstone only)', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_platform_principals_reference_tombstone_only
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION calling_guard_principal_reference();

-- ---------------------------------------------------------------------------
-- Retention lookups (bounded, index-driven sweeps).
-- ---------------------------------------------------------------------------
CREATE INDEX idx_webhook_deliveries_terminal ON webhook_deliveries (completed_at)
    WHERE status IN ('SUCCEEDED', 'FAILED', 'CANCELLED');
CREATE INDEX idx_webhook_deliveries_replay ON webhook_deliveries (replay_of) WHERE replay_of IS NOT NULL;
CREATE INDEX idx_webhook_deliveries_event ON webhook_deliveries (webhook_event_id);
CREATE INDEX idx_webhook_events_created ON webhook_events (created_at);
CREATE INDEX idx_calling_incidents_resolved ON calling_incidents (resolved_at) WHERE status = 'RESOLVED';
CREATE INDEX idx_call_telephony_events_received ON call_telephony_events (received_at);
CREATE INDEX idx_platform_operator_audit_occurred ON platform_operator_audit (occurred_at);
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_platform_operator_audit_occurred;
DROP INDEX IF EXISTS idx_call_telephony_events_received;
DROP INDEX IF EXISTS idx_calling_incidents_resolved;
DROP INDEX IF EXISTS idx_webhook_events_created;
DROP INDEX IF EXISTS idx_webhook_deliveries_event;
DROP INDEX IF EXISTS idx_webhook_deliveries_replay;
DROP INDEX IF EXISTS idx_webhook_deliveries_terminal;

-- Principal tombstone: tombstoned rows keep their (already replaced) reference.
DROP TRIGGER IF EXISTS trg_platform_principals_reference_tombstone_only ON platform_principals;
DROP FUNCTION IF EXISTS calling_guard_principal_reference();
DROP TRIGGER IF EXISTS trg_platform_principals_immutable ON platform_principals;
CREATE TRIGGER trg_platform_principals_immutable
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'kind',
        'external_reference', 'linked_service_account_id', 'created_at');
DROP TRIGGER IF EXISTS trg_platform_principals_tombstoned_once ON platform_principals;
ALTER TABLE platform_principals
    DROP CONSTRAINT IF EXISTS ck_platform_principals_tombstoned,
    DROP COLUMN IF EXISTS tombstoned_at;

-- Minimization (minimized values stay minimized; only the guards go).
DROP INDEX IF EXISTS idx_enterprise_call_contexts_label_unminimized;
DROP TRIGGER IF EXISTS trg_enterprise_call_contexts_entry_label ON enterprise_call_contexts;
DROP FUNCTION IF EXISTS calling_guard_entry_label_minimization();
DROP TRIGGER IF EXISTS trg_enterprise_call_contexts_immutable ON enterprise_call_contexts;
CREATE TRIGGER trg_enterprise_call_contexts_immutable
    BEFORE UPDATE ON enterprise_call_contexts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'call_id', 'tenant_id', 'application_id', 'environment_id', 'entry_extension_id',
        'entry_extension_number', 'entry_label', 'policy_id', 'policy_version', 'max_hops', 'created_at');
DROP INDEX IF EXISTS idx_call_participants_display_unminimized;
DROP TRIGGER IF EXISTS trg_call_participants_display_minimization ON call_participants;
DROP FUNCTION IF EXISTS calling_guard_display_minimization();
DROP TRIGGER IF EXISTS trg_call_participants_display_minimized_once ON call_participants;
ALTER TABLE call_participants
    DROP CONSTRAINT IF EXISTS ck_call_participants_display_minimized,
    DROP COLUMN IF EXISTS display_minimized_at;

DROP TABLE IF EXISTS calling_retention_audit;
DROP FUNCTION IF EXISTS calling_retention_held(text, uuid, uuid, uuid, uuid);
DROP TABLE IF EXISTS calling_retention_holds;

-- Worker flag: back to unconditional guards.
DROP TRIGGER IF EXISTS trg_call_media_provider_events_no_truncate ON call_media_provider_events;
DROP TRIGGER IF EXISTS trg_call_media_provider_events_retention_worker_only ON call_media_provider_events;
DROP TRIGGER IF EXISTS trg_calling_incidents_no_truncate ON calling_incidents;
DROP TRIGGER IF EXISTS trg_calling_incidents_retention_worker_only ON calling_incidents;
DROP TRIGGER IF EXISTS trg_call_telephony_events_no_truncate ON call_telephony_events;
DROP TRIGGER IF EXISTS trg_call_telephony_events_append_only ON call_telephony_events;
CREATE TRIGGER trg_call_telephony_events_append_only
    BEFORE UPDATE OR DELETE ON call_telephony_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
DROP TRIGGER IF EXISTS trg_webhook_delivery_attempts_no_truncate ON webhook_delivery_attempts;
DROP TRIGGER IF EXISTS trg_webhook_delivery_attempts_immutable ON webhook_delivery_attempts;
CREATE TRIGGER trg_webhook_delivery_attempts_immutable
    BEFORE UPDATE OR DELETE ON webhook_delivery_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
DROP TRIGGER IF EXISTS trg_webhook_deliveries_no_truncate ON webhook_deliveries;
DROP TRIGGER IF EXISTS trg_webhook_deliveries_no_delete ON webhook_deliveries;
CREATE TRIGGER trg_webhook_deliveries_no_delete
    BEFORE DELETE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
DROP TRIGGER IF EXISTS trg_webhook_events_no_truncate ON webhook_events;
DROP TRIGGER IF EXISTS trg_webhook_events_immutable ON webhook_events;
CREATE TRIGGER trg_webhook_events_immutable
    BEFORE UPDATE OR DELETE ON webhook_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
DROP TRIGGER IF EXISTS trg_platform_operator_audit_no_update ON platform_operator_audit;
CREATE TRIGGER trg_platform_operator_audit_no_update
    BEFORE UPDATE OR DELETE ON platform_operator_audit
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
DROP FUNCTION IF EXISTS calling_forbid_mutation_outside_retention();

-- Operator audit vocabulary (000038 lists; existing rows are immutable evidence).
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_target_kind;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_target_kind CHECK (target_kind IN (
    'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
    'BLOCK_RULE', 'ABUSE_REPORT', 'COMPLIANCE_ELIGIBILITY')) NOT VALID;
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
    'SPEND_POLICY_SET', 'COMPLIANCE_ELIGIBILITY_REVOKED')) NOT VALID;
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
