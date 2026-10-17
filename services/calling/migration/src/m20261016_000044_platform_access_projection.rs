//! Platform Access cutover (Part IV Workstream B, ADR 0032): Calling's `platform_*`
//! identity tables become a PROJECTION of the shared identity authority.
//!
//! Platform Access (`services/platform-access`) now owns tenants, applications,
//! environments, service accounts, API credentials, scopes and principals and verifies
//! every credential. Calling keeps the same rows — same ids — because every Calling table
//! references them through composite foreign keys, but writes them ONLY by applying
//! Platform Access snapshots (authenticated authority chains and the identity change
//! feed). Calling-owned policy (limits, routing / spend policy, …) is unchanged.
//!
//! * `pa_revision` on every projected entity: the Platform Access revision last applied
//!   (0 = row written by Calling's Part III authority, before the cutover). A snapshot is
//!   applied only when its revision is newer, so out-of-order delivery cannot regress a
//!   row.
//! * Verifier material is no longer required: `verifier`, `verifier_algorithm` and
//!   `verifier_key_version` become nullable (all three or none). Projected credentials
//!   never carry them. Rows exported to Platform Access keep theirs until the hand-off is
//!   verified and completed (`platform_admin identity complete-handoff`), which scrubs
//!   them; `identity restore-verifiers` can put them back from a bundle during a rollback
//!   window. A verifier can be removed or restored, never replaced by another value.
//! * `platform_identity_projection_state`: the change-feed cursor (one row).
//! * `platform_identity_handoffs`: the export / verification / completion record of the
//!   controlled hand-off (digests and counts only — never verifiers).
//! * `platform_auth_failure_windows` is no longer written (failed-authentication
//!   throttling moved to Platform Access with credential verification); it is kept so a
//!   Part III binary still running during the cutover keeps its §109 behaviour.
//! * Operator audit vocabulary: IDENTITY_EXPORTED, IDENTITY_HANDOFF_VERIFIED,
//!   IDENTITY_HANDOFF_COMPLETED, IDENTITY_VERIFIERS_RESTORED on IDENTITY_HANDOFF.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE platform_tenants          ADD COLUMN pa_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE platform_applications     ADD COLUMN pa_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE platform_environments     ADD COLUMN pa_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE platform_service_accounts ADD COLUMN pa_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE platform_api_credentials  ADD COLUMN pa_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE platform_principals       ADD COLUMN pa_revision BIGINT NOT NULL DEFAULT 0;

-- Verifier material becomes optional (projected credentials have none).
ALTER TABLE platform_api_credentials
    ALTER COLUMN verifier DROP NOT NULL,
    ALTER COLUMN verifier_algorithm DROP NOT NULL,
    ALTER COLUMN verifier_key_version DROP NOT NULL,
    DROP CONSTRAINT ck_platform_api_credentials_verifier_length,
    DROP CONSTRAINT ck_platform_api_credentials_verifier_algorithm,
    DROP CONSTRAINT ck_platform_api_credentials_verifier_key_version,
    ADD CONSTRAINT ck_platform_api_credentials_verifier_length
        CHECK (verifier IS NULL OR octet_length(verifier) = 32),
    ADD CONSTRAINT ck_platform_api_credentials_verifier_algorithm
        CHECK (verifier_algorithm IS NULL OR verifier_algorithm IN ('HMAC_SHA256')),
    ADD CONSTRAINT ck_platform_api_credentials_verifier_key_version
        CHECK (verifier_key_version IS NULL OR verifier_key_version > 0),
    ADD CONSTRAINT ck_platform_api_credentials_verifier_complete
        CHECK ((verifier IS NULL) = (verifier_algorithm IS NULL)
               AND (verifier IS NULL) = (verifier_key_version IS NULL));
DROP TRIGGER trg_platform_api_credentials_immutable ON platform_api_credentials;
CREATE TRIGGER trg_platform_api_credentials_immutable
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'public_id', 'tenant_id', 'application_id', 'environment_id', 'service_account_id',
        'last_four', 'not_before', 'expires_at', 'rotated_from_credential_id', 'created_at');
-- A verifier may be scrubbed (value -> NULL) or restored (NULL -> value), never replaced.
CREATE FUNCTION calling_guard_verifier_handoff() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.verifier IS NOT NULL AND NEW.verifier IS NOT NULL
       AND (NEW.verifier IS DISTINCT FROM OLD.verifier
            OR NEW.verifier_algorithm IS DISTINCT FROM OLD.verifier_algorithm
            OR NEW.verifier_key_version IS DISTINCT FROM OLD.verifier_key_version) THEN
        RAISE EXCEPTION 'platform invariant violated: %.verifier can only be scrubbed or restored', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_platform_api_credentials_verifier_handoff
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION calling_guard_verifier_handoff();

CREATE TABLE platform_identity_projection_state (
    id              SMALLINT    NOT NULL DEFAULT 1,
    last_sequence   BIGINT      NOT NULL DEFAULT 0,
    last_synced_at  TIMESTAMPTZ,
    CONSTRAINT pk_platform_identity_projection_state PRIMARY KEY (id),
    CONSTRAINT ck_platform_identity_projection_state_singleton CHECK (id = 1),
    CONSTRAINT ck_platform_identity_projection_state_sequence CHECK (last_sequence >= 0)
);
INSERT INTO platform_identity_projection_state (id, last_sequence) VALUES (1, 0);

CREATE TABLE platform_identity_handoffs (
    id               UUID         NOT NULL,
    bundle_digest    CHAR(64)     NOT NULL,
    verifier_digest  CHAR(64)     NOT NULL,
    counts           JSONB        NOT NULL,
    exported_at      TIMESTAMPTZ  NOT NULL,
    exported_by      VARCHAR(100) NOT NULL,
    verified_at      TIMESTAMPTZ,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_platform_identity_handoffs PRIMARY KEY (id),
    CONSTRAINT uq_platform_identity_handoffs_digest UNIQUE (bundle_digest),
    CONSTRAINT ck_platform_identity_handoffs_digests CHECK (
        bundle_digest ~ '^[0-9a-f]{64}$' AND verifier_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_platform_identity_handoffs_counts CHECK (jsonb_typeof(counts) = 'object'),
    CONSTRAINT ck_platform_identity_handoffs_order CHECK (
        (completed_at IS NULL OR verified_at IS NOT NULL)
        AND (verified_at IS NULL OR verified_at >= exported_at)
        AND (completed_at IS NULL OR completed_at >= verified_at))
);
CREATE TRIGGER trg_platform_identity_handoffs_immutable
    BEFORE UPDATE ON platform_identity_handoffs
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'bundle_digest', 'verifier_digest', 'counts', 'exported_at', 'exported_by');
CREATE TRIGGER trg_platform_identity_handoffs_set_once
    BEFORE UPDATE ON platform_identity_handoffs
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('verified_at', 'completed_at');

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
    'WEBHOOK_KEYS_RESEALED', 'INCIDENT_RESOLVED',
    'IDENTITY_EXPORTED', 'IDENTITY_HANDOFF_VERIFIED', 'IDENTITY_HANDOFF_COMPLETED',
    'IDENTITY_VERIFIERS_RESTORED'));
ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_target_kind;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_target_kind CHECK (target_kind IN (
    'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
    'BLOCK_RULE', 'ABUSE_REPORT', 'COMPLIANCE_ELIGIBILITY', 'RETENTION_HOLD',
    'WEBHOOK_KEY_RING', 'INCIDENT', 'IDENTITY_HANDOFF'));
"#;

/// Down restores the Part III shape. It refuses (and changes nothing) while any
/// credential has no verifier: such a credential could not authenticate under the Part
/// III authority — restore the verifiers from a Platform Access export first
/// (`platform_admin identity restore-verifiers`). The operator audit is append-only, so
/// the narrowed vocabulary is restored `NOT VALID` (recorded hand-off rows stay).
const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM platform_api_credentials WHERE verifier IS NULL) THEN
        RAISE EXCEPTION 'cannot roll back the Platform Access cutover: credentials without verifiers exist (restore them from a Platform Access export first)'
            USING ERRCODE = 'object_not_in_prerequisite_state';
    END IF;
END;
$$;

ALTER TABLE platform_operator_audit DROP CONSTRAINT ck_platform_operator_audit_target_kind;
ALTER TABLE platform_operator_audit ADD CONSTRAINT ck_platform_operator_audit_target_kind CHECK (target_kind IN (
    'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
    'BLOCK_RULE', 'ABUSE_REPORT', 'COMPLIANCE_ELIGIBILITY', 'RETENTION_HOLD',
    'WEBHOOK_KEY_RING', 'INCIDENT')) NOT VALID;
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

DROP TABLE IF EXISTS platform_identity_handoffs;
DROP TABLE IF EXISTS platform_identity_projection_state;

DROP TRIGGER IF EXISTS trg_platform_api_credentials_verifier_handoff ON platform_api_credentials;
DROP FUNCTION IF EXISTS calling_guard_verifier_handoff();
DROP TRIGGER trg_platform_api_credentials_immutable ON platform_api_credentials;
CREATE TRIGGER trg_platform_api_credentials_immutable
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'public_id', 'tenant_id', 'application_id', 'environment_id', 'service_account_id',
        'verifier', 'verifier_algorithm', 'verifier_key_version', 'last_four',
        'not_before', 'expires_at', 'rotated_from_credential_id', 'created_at');
ALTER TABLE platform_api_credentials
    DROP CONSTRAINT ck_platform_api_credentials_verifier_complete,
    DROP CONSTRAINT ck_platform_api_credentials_verifier_key_version,
    DROP CONSTRAINT ck_platform_api_credentials_verifier_algorithm,
    DROP CONSTRAINT ck_platform_api_credentials_verifier_length,
    ADD CONSTRAINT ck_platform_api_credentials_verifier_length CHECK (octet_length(verifier) = 32),
    ADD CONSTRAINT ck_platform_api_credentials_verifier_algorithm CHECK (verifier_algorithm IN ('HMAC_SHA256')),
    ADD CONSTRAINT ck_platform_api_credentials_verifier_key_version CHECK (verifier_key_version > 0),
    ALTER COLUMN verifier SET NOT NULL,
    ALTER COLUMN verifier_algorithm SET NOT NULL,
    ALTER COLUMN verifier_key_version SET NOT NULL;

ALTER TABLE platform_principals       DROP COLUMN pa_revision;
ALTER TABLE platform_api_credentials  DROP COLUMN pa_revision;
ALTER TABLE platform_service_accounts DROP COLUMN pa_revision;
ALTER TABLE platform_environments     DROP COLUMN pa_revision;
ALTER TABLE platform_applications     DROP COLUMN pa_revision;
ALTER TABLE platform_tenants          DROP COLUMN pa_revision;
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
