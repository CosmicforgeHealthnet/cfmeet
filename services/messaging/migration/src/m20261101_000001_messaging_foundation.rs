//! Messaging foundation (Part V Workstream C). Every table is `messaging_`-prefixed and
//! owned by this service only. Platform identities are opaque Platform Access ids.
//!
//! * `messaging_device_challenges` — single-use registration challenges: a server nonce
//!   and a server-assigned device id, bound to the authorizing credential, principal and
//!   application environment, with a short expiry.
//! * `messaging_devices` — Messaging devices: the device signature PUBLIC key (Ed25519,
//!   32 bytes; unique for ever), its MLS credential identity, status ACTIVE → REVOKED
//!   (one way; trigger), the transparency log leaf of its registration.
//! * `messaging_key_packages` — published MLS KeyPackages (PUBLIC material, sensitive
//!   protocol objects): unique RFC 9420 KeyPackageRef (replay prevention), bounded size and
//!   lifetime, status AVAILABLE → RESERVED → CONSUMED, or REVOKED / EXPIRED (trigger).
//! * `messaging_transparency_*` — the append-only RFC 9162 Merkle log of device key
//!   events: leaves, stored perfect subtrees and the single tree head row that serializes
//!   appends. Updates and deletes are refused.
//! * `messaging_rate_windows` — fixed-window counters (registration challenges,
//!   directory lookups).
//! * `messaging_audit` — append-only record of key / device changes.
//! * `messaging_outbox_events` — the transactional outbox (lease, bounded retry,
//!   DEAD_LETTER); payloads may never carry key material, ciphertext or content.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE FUNCTION messaging_forbid_mutation() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'messaging: % on % is not allowed (append-only)', TG_OP, TG_TABLE_NAME
        USING ERRCODE = 'P0001';
END $$;

-- ---------------------------------------------------------------------------------------
-- Device registration challenges
-- ---------------------------------------------------------------------------------------
CREATE TABLE messaging_device_challenges (
    id              UUID         NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    principal_id    UUID         NOT NULL,
    credential_id   UUID         NOT NULL,
    device_id       UUID         NOT NULL,
    nonce           BYTEA        NOT NULL,
    status          VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    created_at      TIMESTAMPTZ  NOT NULL,
    expires_at      TIMESTAMPTZ  NOT NULL,
    consumed_at     TIMESTAMPTZ,
    CONSTRAINT pk_messaging_device_challenges PRIMARY KEY (id),
    CONSTRAINT uq_messaging_device_challenges_device UNIQUE (device_id),
    CONSTRAINT uq_messaging_device_challenges_nonce UNIQUE (nonce),
    CONSTRAINT ck_messaging_device_challenges_nonce CHECK (octet_length(nonce) = 32),
    CONSTRAINT ck_messaging_device_challenges_status CHECK (status IN ('PENDING', 'CONSUMED')),
    CONSTRAINT ck_messaging_device_challenges_consumed CHECK ((status = 'CONSUMED') = (consumed_at IS NOT NULL)),
    CONSTRAINT ck_messaging_device_challenges_expiry CHECK (expires_at > created_at
        AND expires_at <= created_at + INTERVAL '1 hour')
);
CREATE INDEX ix_messaging_device_challenges_principal
    ON messaging_device_challenges (environment_id, principal_id, created_at DESC);

-- ---------------------------------------------------------------------------------------
-- Devices (public key material only)
-- ---------------------------------------------------------------------------------------
CREATE TABLE messaging_devices (
    id                          UUID         NOT NULL,
    tenant_id                   UUID         NOT NULL,
    application_id              UUID         NOT NULL,
    environment_id              UUID         NOT NULL,
    principal_id                UUID         NOT NULL,
    status                      VARCHAR(16)  NOT NULL,
    protocol                    VARCHAR(16)  NOT NULL,
    ciphersuite                 VARCHAR(64)  NOT NULL,
    signature_public_key        BYTEA        NOT NULL,
    credential_identity         BYTEA        NOT NULL,
    key_fingerprint             CHAR(64)     NOT NULL,
    display_label               VARCHAR(64),
    registered_by_credential_id UUID         NOT NULL,
    challenge_id                UUID         NOT NULL,
    transparency_leaf_index     BIGINT       NOT NULL,
    revocation_leaf_index       BIGINT,
    registered_at               TIMESTAMPTZ  NOT NULL,
    revoked_at                  TIMESTAMPTZ,
    revocation_reason           VARCHAR(32),
    version                     BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_messaging_devices PRIMARY KEY (id),
    CONSTRAINT fk_messaging_devices_challenge FOREIGN KEY (challenge_id)
        REFERENCES messaging_device_challenges (id),
    CONSTRAINT uq_messaging_devices_challenge UNIQUE (challenge_id),
    -- A device signature key identifies exactly one device, for ever (no re-use after
    -- revocation, no sharing across principals or tenants).
    CONSTRAINT uq_messaging_devices_signature_key UNIQUE (signature_public_key),
    CONSTRAINT uq_messaging_devices_scope UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_messaging_devices_leaf UNIQUE (transparency_leaf_index),
    CONSTRAINT ck_messaging_devices_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_messaging_devices_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)
        AND (status = 'REVOKED') = (revocation_reason IS NOT NULL)
        AND (status = 'REVOKED') = (revocation_leaf_index IS NOT NULL)),
    CONSTRAINT ck_messaging_devices_protocol CHECK (protocol = 'MLS_1_0'),
    CONSTRAINT ck_messaging_devices_ciphersuite CHECK (ciphersuite = 'MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519'),
    CONSTRAINT ck_messaging_devices_signature_key CHECK (octet_length(signature_public_key) = 32),
    CONSTRAINT ck_messaging_devices_identity CHECK (octet_length(credential_identity) BETWEEN 1 AND 128),
    CONSTRAINT ck_messaging_devices_fingerprint CHECK (key_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_devices_label CHECK (display_label IS NULL
        OR (length(display_label) BETWEEN 1 AND 64 AND display_label !~ '[[:cntrl:]]')),
    CONSTRAINT ck_messaging_devices_reason CHECK (revocation_reason IS NULL
        OR revocation_reason IN ('USER_REQUESTED', 'LOST_OR_STOLEN', 'COMPROMISED', 'REPLACED', 'ADMINISTRATIVE')),
    CONSTRAINT ck_messaging_devices_version CHECK (version >= 1)
);
CREATE INDEX ix_messaging_devices_principal_active
    ON messaging_devices (environment_id, principal_id) WHERE status = 'ACTIVE';

-- Key fields never change; REVOKED is terminal; the version advances by exactly one.
CREATE FUNCTION messaging_devices_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: devices are never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.principal_id <> OLD.principal_id
       OR NEW.signature_public_key <> OLD.signature_public_key
       OR NEW.credential_identity <> OLD.credential_identity
       OR NEW.key_fingerprint <> OLD.key_fingerprint OR NEW.protocol <> OLD.protocol
       OR NEW.ciphersuite <> OLD.ciphersuite OR NEW.challenge_id <> OLD.challenge_id
       OR NEW.transparency_leaf_index <> OLD.transparency_leaf_index
       OR NEW.registered_at <> OLD.registered_at THEN
        RAISE EXCEPTION 'messaging: device key facts are immutable' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.status = 'REVOKED' THEN
        RAISE EXCEPTION 'messaging: a revoked device is terminal' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'messaging: device version must advance by one' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_devices_guard BEFORE UPDATE OR DELETE ON messaging_devices
    FOR EACH ROW EXECUTE FUNCTION messaging_devices_guard();

-- ---------------------------------------------------------------------------------------
-- Published KeyPackages (public MLS material)
-- ---------------------------------------------------------------------------------------
CREATE TABLE messaging_key_packages (
    id               UUID         NOT NULL,
    device_id        UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    key_package_ref  BYTEA        NOT NULL,
    encoded          BYTEA        NOT NULL,
    status           VARCHAR(16)  NOT NULL DEFAULT 'AVAILABLE',
    not_before       TIMESTAMPTZ  NOT NULL,
    not_after        TIMESTAMPTZ  NOT NULL,
    published_at     TIMESTAMPTZ  NOT NULL,
    reservation_id   UUID,
    reserved_at      TIMESTAMPTZ,
    consumed_at      TIMESTAMPTZ,
    closed_at        TIMESTAMPTZ,
    CONSTRAINT pk_messaging_key_packages PRIMARY KEY (id),
    CONSTRAINT fk_messaging_key_packages_device FOREIGN KEY (tenant_id, application_id, environment_id, device_id)
        REFERENCES messaging_devices (tenant_id, application_id, environment_id, id),
    -- RFC 9420 KeyPackageRef: a KeyPackage is accepted once, ever (replay prevention).
    CONSTRAINT uq_messaging_key_packages_ref UNIQUE (key_package_ref),
    CONSTRAINT ck_messaging_key_packages_ref CHECK (octet_length(key_package_ref) = 32),
    CONSTRAINT ck_messaging_key_packages_size CHECK (octet_length(encoded) BETWEEN 64 AND 16384),
    CONSTRAINT ck_messaging_key_packages_status CHECK (status IN ('AVAILABLE', 'RESERVED', 'CONSUMED', 'REVOKED', 'EXPIRED')),
    CONSTRAINT ck_messaging_key_packages_lifetime CHECK (not_after > not_before
        AND not_after - not_before <= INTERVAL '91 days'),
    CONSTRAINT ck_messaging_key_packages_reserved CHECK (status NOT IN ('RESERVED', 'CONSUMED')
        OR (reservation_id IS NOT NULL AND reserved_at IS NOT NULL)),
    CONSTRAINT ck_messaging_key_packages_available CHECK (status <> 'AVAILABLE'
        OR (reservation_id IS NULL AND reserved_at IS NULL AND closed_at IS NULL)),
    CONSTRAINT ck_messaging_key_packages_closed CHECK ((status IN ('REVOKED', 'EXPIRED')) = (closed_at IS NOT NULL)),
    CONSTRAINT ck_messaging_key_packages_consumed CHECK ((status = 'CONSUMED') = (consumed_at IS NOT NULL))
);
CREATE INDEX ix_messaging_key_packages_available
    ON messaging_key_packages (device_id, not_after) WHERE status = 'AVAILABLE';
CREATE INDEX ix_messaging_key_packages_reservation
    ON messaging_key_packages (reservation_id) WHERE reservation_id IS NOT NULL;

-- AVAILABLE -> RESERVED -> CONSUMED; AVAILABLE / RESERVED -> REVOKED / EXPIRED; nothing
-- else, and the published object never changes.
CREATE FUNCTION messaging_key_packages_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: KeyPackages are closed, never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.encoded <> OLD.encoded OR NEW.key_package_ref <> OLD.key_package_ref
       OR NEW.device_id <> OLD.device_id OR NEW.principal_id <> OLD.principal_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.not_after <> OLD.not_after
       OR NEW.not_before <> OLD.not_before THEN
        RAISE EXCEPTION 'messaging: a published KeyPackage is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF NOT ((OLD.status = NEW.status)
         OR (OLD.status = 'AVAILABLE' AND NEW.status IN ('RESERVED', 'REVOKED', 'EXPIRED'))
         OR (OLD.status = 'RESERVED' AND NEW.status IN ('CONSUMED', 'REVOKED', 'EXPIRED'))) THEN
        RAISE EXCEPTION 'messaging: KeyPackage transition % -> % refused', OLD.status, NEW.status
            USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_key_packages_guard BEFORE UPDATE OR DELETE ON messaging_key_packages
    FOR EACH ROW EXECUTE FUNCTION messaging_key_packages_guard();

-- ---------------------------------------------------------------------------------------
-- Key-transparency log (RFC 9162 Merkle tree; append-only)
-- ---------------------------------------------------------------------------------------
CREATE TABLE messaging_transparency_head (
    id          SMALLINT     NOT NULL,
    tree_size   BIGINT       NOT NULL,
    root_hash   BYTEA        NOT NULL,
    updated_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_transparency_head PRIMARY KEY (id),
    CONSTRAINT ck_messaging_transparency_head_single CHECK (id = 1),
    CONSTRAINT ck_messaging_transparency_head_size CHECK (tree_size >= 0),
    CONSTRAINT ck_messaging_transparency_head_root CHECK (octet_length(root_hash) = 32)
);
-- The empty tree: SHA-256("").
INSERT INTO messaging_transparency_head (id, tree_size, root_hash, updated_at)
VALUES (1, 0, decode('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855', 'hex'), now());

CREATE FUNCTION messaging_transparency_head_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' OR NEW.tree_size < OLD.tree_size THEN
        RAISE EXCEPTION 'messaging: the transparency log only grows' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_transparency_head_guard BEFORE UPDATE OR DELETE ON messaging_transparency_head
    FOR EACH ROW EXECUTE FUNCTION messaging_transparency_head_guard();

CREATE TABLE messaging_transparency_leaves (
    leaf_index      BIGINT       NOT NULL,
    entry_kind      VARCHAR(24)  NOT NULL,
    environment_id  UUID         NOT NULL,
    principal_id    UUID         NOT NULL,
    device_id       UUID         NOT NULL,
    leaf_data       BYTEA        NOT NULL,
    leaf_hash       BYTEA        NOT NULL,
    appended_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_transparency_leaves PRIMARY KEY (leaf_index),
    CONSTRAINT ck_messaging_transparency_leaves_index CHECK (leaf_index >= 0),
    CONSTRAINT ck_messaging_transparency_leaves_kind CHECK (entry_kind IN ('DEVICE_REGISTERED', 'DEVICE_REVOKED')),
    CONSTRAINT ck_messaging_transparency_leaves_data CHECK (octet_length(leaf_data) BETWEEN 1 AND 2048),
    CONSTRAINT ck_messaging_transparency_leaves_hash CHECK (octet_length(leaf_hash) = 32)
);
CREATE INDEX ix_messaging_transparency_leaves_device ON messaging_transparency_leaves (device_id, leaf_index);
CREATE TRIGGER trg_messaging_transparency_leaves_append_only BEFORE UPDATE OR DELETE ON messaging_transparency_leaves
    FOR EACH ROW EXECUTE FUNCTION messaging_forbid_mutation();

CREATE TABLE messaging_transparency_nodes (
    level       SMALLINT  NOT NULL,
    node_index  BIGINT    NOT NULL,
    hash        BYTEA     NOT NULL,
    CONSTRAINT pk_messaging_transparency_nodes PRIMARY KEY (level, node_index),
    CONSTRAINT ck_messaging_transparency_nodes_level CHECK (level BETWEEN 0 AND 62),
    CONSTRAINT ck_messaging_transparency_nodes_index CHECK (node_index >= 0),
    CONSTRAINT ck_messaging_transparency_nodes_hash CHECK (octet_length(hash) = 32)
);
CREATE TRIGGER trg_messaging_transparency_nodes_append_only BEFORE UPDATE OR DELETE ON messaging_transparency_nodes
    FOR EACH ROW EXECUTE FUNCTION messaging_forbid_mutation();

-- ---------------------------------------------------------------------------------------
-- Rate windows, audit
-- ---------------------------------------------------------------------------------------
CREATE TABLE messaging_rate_windows (
    window_key    VARCHAR(160) NOT NULL,
    window_start  TIMESTAMPTZ  NOT NULL,
    count         BIGINT       NOT NULL,
    CONSTRAINT pk_messaging_rate_windows PRIMARY KEY (window_key, window_start),
    CONSTRAINT ck_messaging_rate_windows_count CHECK (count >= 0)
);

CREATE TABLE messaging_audit (
    id              UUID         NOT NULL,
    occurred_at     TIMESTAMPTZ  NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    credential_id   UUID,
    principal_id    UUID,
    action          VARCHAR(48)  NOT NULL,
    target_kind     VARCHAR(32)  NOT NULL,
    target_id       UUID         NOT NULL,
    correlation_id  VARCHAR(64)  NOT NULL,
    detail          JSONB        NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT pk_messaging_audit PRIMARY KEY (id),
    CONSTRAINT ck_messaging_audit_action CHECK (action ~ '^[A-Z][A-Z_]{2,47}$'),
    CONSTRAINT ck_messaging_audit_target CHECK (target_kind ~ '^[A-Z][A-Z_]{2,31}$'),
    CONSTRAINT ck_messaging_audit_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_messaging_audit_detail CHECK (jsonb_typeof(detail) = 'object'
        AND octet_length(detail::text) <= 2048
        AND detail::text !~* '"[a-z_]*(secret|private|token|credential_value|ciphertext|plaintext|content|body)[a-z_]*":')
);
CREATE INDEX ix_messaging_audit_target ON messaging_audit (target_kind, target_id, occurred_at DESC);
CREATE INDEX ix_messaging_audit_environment ON messaging_audit (environment_id, occurred_at DESC);
CREATE TRIGGER trg_messaging_audit_append_only BEFORE UPDATE OR DELETE ON messaging_audit
    FOR EACH ROW EXECUTE FUNCTION messaging_forbid_mutation();

-- ---------------------------------------------------------------------------------------
-- Transactional outbox
-- ---------------------------------------------------------------------------------------
CREATE TABLE messaging_outbox_events (
    id               UUID         NOT NULL,
    sequence         BIGSERIAL    NOT NULL,
    event_type       VARCHAR(80)  NOT NULL,
    schema_version   SMALLINT     NOT NULL DEFAULT 1,
    aggregate_type   VARCHAR(40)  NOT NULL,
    aggregate_id     UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    correlation_id   VARCHAR(64)  NOT NULL,
    causation_id     VARCHAR(64),
    payload          JSONB        NOT NULL,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    status           VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_token      UUID,
    lease_expires_at TIMESTAMPTZ,
    last_error       VARCHAR(200),
    published_at     TIMESTAMPTZ,
    CONSTRAINT pk_messaging_outbox_events PRIMARY KEY (id),
    CONSTRAINT uq_messaging_outbox_events_sequence UNIQUE (sequence),
    CONSTRAINT ck_messaging_outbox_events_type CHECK (event_type ~ '^messaging(\.[a-z][a-z0-9-]*){2,}$'
        AND length(event_type) <= 80),
    CONSTRAINT ck_messaging_outbox_events_version CHECK (schema_version >= 1),
    CONSTRAINT ck_messaging_outbox_events_aggregate CHECK (aggregate_type ~ '^[A-Z][A-Z_]{1,39}$'),
    CONSTRAINT ck_messaging_outbox_events_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_messaging_outbox_events_causation CHECK (causation_id IS NULL OR causation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    -- Events carry ids, states and versions: never key material, ciphertext, content or a
    -- credential.
    CONSTRAINT ck_messaging_outbox_events_payload CHECK (jsonb_typeof(payload) = 'object'
        AND octet_length(payload::text) <= 4096
        AND payload::text !~* '"[a-z_]*(secret|private|password|token|credential_value|api_?key|ciphertext|plaintext|content|body|text|key_package|signature_key|phone|msisdn|e164)[a-z_]*":'),
    CONSTRAINT ck_messaging_outbox_events_status CHECK (status IN ('PENDING', 'PUBLISHED', 'DEAD_LETTER')),
    CONSTRAINT ck_messaging_outbox_events_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_messaging_outbox_events_published CHECK ((status = 'PUBLISHED') = (published_at IS NOT NULL)),
    CONSTRAINT ck_messaging_outbox_events_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL))
);
CREATE INDEX ix_messaging_outbox_events_due ON messaging_outbox_events (next_attempt_at, sequence) WHERE status = 'PENDING';
CREATE INDEX ix_messaging_outbox_events_aggregate ON messaging_outbox_events (aggregate_type, aggregate_id, sequence) WHERE status = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_outbox_events;
DROP TABLE IF EXISTS messaging_audit;
DROP TABLE IF EXISTS messaging_rate_windows;
DROP TABLE IF EXISTS messaging_transparency_nodes;
DROP TABLE IF EXISTS messaging_transparency_leaves;
DROP TABLE IF EXISTS messaging_transparency_head;
DROP FUNCTION IF EXISTS messaging_transparency_head_guard();
DROP TABLE IF EXISTS messaging_key_packages;
DROP FUNCTION IF EXISTS messaging_key_packages_guard();
DROP TABLE IF EXISTS messaging_devices;
DROP FUNCTION IF EXISTS messaging_devices_guard();
DROP TABLE IF EXISTS messaging_device_challenges;
DROP FUNCTION IF EXISTS messaging_forbid_mutation();
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
