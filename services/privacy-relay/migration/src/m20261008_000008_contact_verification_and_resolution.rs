//! Contact verification and execution resolution (Part D §33).
//!
//! * `contact_verification_events`: append-only history of every verification state
//!   change (trusted assertion or expiry), with its trust source, opaque evidence
//!   reference, policy version, validity, actor and request ids. Never a number.
//! * `contact_resolution_grants`: a short-lived (at most 300 s), single-execution grant
//!   binding ONE protected contact to one authorized call leg — scope, Communication
//!   Context, Privacy Relay Session, authorization decision and the versions it was
//!   decided at, Calling's call and call-leg ids, purpose. Only the SHA-256 of the grant
//!   secret is stored. Consumption records the resolving service and execution attempt
//!   (idempotent retry of the same attempt only).
//! * Authorization reasons gain the contact verification outcomes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE contact_verification_events (
    id                   UUID PRIMARY KEY,
    contact_reference    UUID         NOT NULL,
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    contact_version      BIGINT       NOT NULL,
    from_state           TEXT         NOT NULL,
    to_state             TEXT         NOT NULL,
    trust_source         VARCHAR(64)  NOT NULL,
    evidence_reference   VARCHAR(128),
    policy_version       VARCHAR(32)  NOT NULL,
    valid_until          TIMESTAMPTZ,
    actor_kind           TEXT         NOT NULL,
    actor_service        VARCHAR(64)  NOT NULL,
    service_account_id   UUID,
    request_id           VARCHAR(36)  NOT NULL,
    correlation_id       VARCHAR(37)  NOT NULL,
    occurred_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT fk_contact_verification_contact FOREIGN KEY (tenant_id, application_id, environment_id, contact_reference)
        REFERENCES protected_contacts (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_contact_verification_states CHECK (
        from_state IN ('UNVERIFIED', 'PENDING', 'VERIFIED', 'EXPIRED', 'INVALID', 'BLOCKED')
        AND to_state IN ('UNVERIFIED', 'PENDING', 'VERIFIED', 'EXPIRED', 'INVALID', 'BLOCKED')),
    CONSTRAINT ck_contact_verification_trust CHECK (trust_source ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_contact_verification_evidence CHECK (
        evidence_reference IS NULL OR privacy_relay_safe_reference(evidence_reference)),
    CONSTRAINT ck_contact_verification_validity CHECK (valid_until IS NULL OR to_state = 'VERIFIED'),
    CONSTRAINT ck_contact_verification_actor CHECK (actor_kind IN ('DELEGATED_CLIENT', 'SERVICE', 'SYSTEM')),
    CONSTRAINT ck_contact_verification_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
CREATE INDEX idx_contact_verification_contact ON contact_verification_events (contact_reference, occurred_at);
CREATE TRIGGER trg_contact_verification_append_only
    BEFORE UPDATE OR DELETE ON contact_verification_events
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

CREATE TABLE contact_resolution_grants (
    id                     UUID PRIMARY KEY,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    contact_reference      UUID         NOT NULL,
    context_ref            UUID         NOT NULL,
    relay_session_ref      UUID         NOT NULL,
    decision_ref           UUID         NOT NULL,
    context_version        BIGINT       NOT NULL,
    session_version        BIGINT       NOT NULL,
    call_ref               UUID         NOT NULL,
    call_leg_ref           UUID         NOT NULL,
    purpose                TEXT         NOT NULL,
    secret_hash            CHAR(64)     NOT NULL,
    status                 TEXT         NOT NULL,
    requested_by_service   VARCHAR(64)  NOT NULL,
    issued_at              TIMESTAMPTZ  NOT NULL,
    expires_at             TIMESTAMPTZ  NOT NULL,
    consumed_at            TIMESTAMPTZ,
    consumed_by_service    VARCHAR(64),
    consumed_attempt_ref   UUID,
    revoked_at             TIMESTAMPTZ,
    request_id             VARCHAR(36)  NOT NULL,
    correlation_id         VARCHAR(37)  NOT NULL,
    CONSTRAINT fk_contact_grants_contact FOREIGN KEY (tenant_id, application_id, environment_id, contact_reference)
        REFERENCES protected_contacts (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_contact_grants_decision FOREIGN KEY (decision_ref) REFERENCES relay_authorization_decisions (id),
    CONSTRAINT ck_contact_grants_purpose CHECK (purpose IN ('PUBLIC_NETWORK_CALL_LEG')),
    CONSTRAINT ck_contact_grants_status CHECK (status IN ('ISSUED', 'CONSUMED', 'REVOKED', 'EXPIRED')),
    CONSTRAINT ck_contact_grants_secret CHECK (secret_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_contact_grants_ttl CHECK (expires_at > issued_at AND expires_at <= issued_at + interval '300 seconds'),
    CONSTRAINT ck_contact_grants_consumed CHECK (
        (status = 'CONSUMED') = (consumed_at IS NOT NULL)
        AND (consumed_at IS NULL) = (consumed_by_service IS NULL)
        AND (consumed_at IS NULL) = (consumed_attempt_ref IS NULL)),
    CONSTRAINT ck_contact_grants_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_contact_grants_versions CHECK (context_version >= 1 AND session_version >= 1),
    CONSTRAINT ck_contact_grants_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
-- At most one live grant per call leg and contact.
CREATE UNIQUE INDEX uq_contact_grants_live_leg ON contact_resolution_grants (call_leg_ref, contact_reference)
    WHERE status = 'ISSUED';
CREATE INDEX idx_contact_grants_expiry ON contact_resolution_grants (status, expires_at);
CREATE INDEX idx_contact_grants_contact ON contact_resolution_grants (contact_reference);
CREATE INDEX idx_contact_grants_call ON contact_resolution_grants (call_ref, call_leg_ref);
CREATE TRIGGER trg_contact_grants_immutable
    BEFORE UPDATE ON contact_resolution_grants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'contact_reference', 'context_ref',
        'relay_session_ref', 'decision_ref', 'context_version', 'session_version', 'call_ref', 'call_leg_ref',
        'purpose', 'secret_hash', 'requested_by_service', 'issued_at', 'expires_at', 'request_id', 'correlation_id');
CREATE TRIGGER trg_contact_grants_set_once
    BEFORE UPDATE ON contact_resolution_grants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment(
        'consumed_at', 'consumed_by_service', 'consumed_attempt_ref', 'revoked_at');
CREATE TRIGGER trg_contact_grants_no_delete
    BEFORE DELETE ON contact_resolution_grants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

ALTER TABLE relay_authorization_decisions DROP CONSTRAINT ck_relay_decisions_reason;
ALTER TABLE relay_authorization_decisions ADD CONSTRAINT ck_relay_decisions_reason CHECK (reason IN (
    'AUTHORIZED', 'FEATURE_DISABLED', 'CONTEXT_NOT_ACTIVE', 'CONTEXT_NOT_STARTED', 'CONTEXT_EXPIRED',
    'CONTEXT_REVOKED', 'CONTEXT_SUSPENDED', 'SESSION_NOT_ACTIVE', 'SESSION_NOT_STARTED', 'SESSION_EXPIRED',
    'PARTICIPANT_NOT_AUTHORIZED', 'DIRECTION_NOT_ALLOWED', 'CHANNEL_NOT_ALLOWED', 'STRATEGY_NOT_ALLOWED',
    'GRACE_POLICY_DENIED', 'CAPABILITY_UNAVAILABLE', 'CONTACT_NOT_VERIFIED', 'CONTACT_VERIFICATION_EXPIRED',
    'CONTACT_BLOCKED'));
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS contact_resolution_grants;
DROP TABLE IF EXISTS contact_verification_events;
-- Decisions are immutable evidence: the narrower constraint is restored for new rows only.
ALTER TABLE relay_authorization_decisions DROP CONSTRAINT ck_relay_decisions_reason;
ALTER TABLE relay_authorization_decisions ADD CONSTRAINT ck_relay_decisions_reason CHECK (reason IN (
    'AUTHORIZED', 'FEATURE_DISABLED', 'CONTEXT_NOT_ACTIVE', 'CONTEXT_NOT_STARTED', 'CONTEXT_EXPIRED',
    'CONTEXT_REVOKED', 'CONTEXT_SUSPENDED', 'SESSION_NOT_ACTIVE', 'SESSION_NOT_STARTED', 'SESSION_EXPIRED',
    'PARTICIPANT_NOT_AUTHORIZED', 'DIRECTION_NOT_ALLOWED', 'CHANNEL_NOT_ALLOWED', 'STRATEGY_NOT_ALLOWED',
    'GRACE_POLICY_DENIED', 'CAPABILITY_UNAVAILABLE')) NOT VALID;
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
