//! ADR 0068, phase A6 — pricing and plans (design §8), owned by the Developer Platform
//! and administered ONLY through the HQ admin bridge (`pricing.administer`). Additive.
//!
//! * `devplat_price_books`: one lineage per product line (`PARTNER_PLATFORM` |
//!   `CONSUMER`), currency and market. Two lineages are seeded (USD, market `GLOBAL`);
//!   their versions start empty (HQ drafts the first).
//! * `devplat_price_versions`: DRAFT → SUBMITTED → APPROVED | REJECTED → SUPERSEDED. The
//!   rates are frozen once a version leaves DRAFT (trigger), their SHA-256 digest is kept
//!   at submission and approval; an approval expires after 12 hours.
//! * `devplat_price_activations`: the append-only activation history (scheduled,
//!   activated, rolled back), each with its `effective_from`; the effective version of a
//!   lineage is the latest activation already effective.
//! * `devplat_market_freezes`: a frozen market refuses every price change.
//! * `devplat_plan_holders` / `devplat_plan_assignments_consumer`: the CF Meet user plan of
//!   a user or business organization (opaque ids; none: Free) and its append-only history;
//!   a billing-account reference is kept ready for a future billing provider.
//! * `devplat_plan_overrides`: per-holder limit overrides.
//!
//! Nothing here charges anyone: amounts are in minor units of the lineage's currency,
//! there is no card, tax or balance. Rollback: DOWN drops the tables and restores
//! the A4 subject check (`NOT VALID`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE devplat_admin_operations DROP CONSTRAINT ck_devplat_admin_operations_subject;
ALTER TABLE devplat_admin_operations ADD CONSTRAINT ck_devplat_admin_operations_subject CHECK (subject_kind IN
    ('PRODUCTION_ACCESS', 'APP', 'ORGANIZATION', 'SUPPORT_PROFILE', 'PUSH_KEY', 'WEBHOOK_DELIVERY', 'USAGE_ROLLUPS',
     'PRICE_BOOK', 'PRICE_VERSION', 'MARKET', 'PLAN_HOLDER', 'PLAN_OVERRIDE'));

CREATE TABLE devplat_price_books (
    id            UUID         NOT NULL,
    product_line  VARCHAR(24)  NOT NULL,
    currency      CHAR(3)      NOT NULL,
    market_code   VARCHAR(16)  NOT NULL,
    created_at    TIMESTAMPTZ  NOT NULL,
    updated_at    TIMESTAMPTZ  NOT NULL,
    version       BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_price_books PRIMARY KEY (id),
    CONSTRAINT uq_devplat_price_books_lineage UNIQUE (product_line, currency, market_code),
    CONSTRAINT ck_devplat_price_books_line CHECK (product_line IN ('PARTNER_PLATFORM', 'CONSUMER')),
    CONSTRAINT ck_devplat_price_books_currency CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_devplat_price_books_market CHECK (market_code ~ '^[A-Z][A-Z0-9_]{1,15}$'),
    CONSTRAINT ck_devplat_price_books_version CHECK (version >= 1)
);
INSERT INTO devplat_price_books (id, product_line, currency, market_code, created_at, updated_at) VALUES
    ('6c0d4a52-5d9e-4f53-9a51-8e0c1f6f0a01', 'PARTNER_PLATFORM', 'USD', 'GLOBAL', now(), now()),
    ('6c0d4a52-5d9e-4f53-9a51-8e0c1f6f0a02', 'CONSUMER', 'USD', 'GLOBAL', now(), now());

CREATE TABLE devplat_price_versions (
    id                  UUID          NOT NULL,
    price_book_id       UUID          NOT NULL,
    seq                 INTEGER       NOT NULL,
    state               VARCHAR(16)   NOT NULL,
    rates               JSONB         NOT NULL,
    rates_digest        CHAR(64)      NOT NULL,
    submitted_digest    CHAR(64),
    drafted_by          VARCHAR(160)  NOT NULL,
    submitted_by        VARCHAR(160),
    submitted_at        TIMESTAMPTZ,
    approved_by         VARCHAR(160),
    approver_ref        VARCHAR(160),
    approval_ref        VARCHAR(128),
    approved_at         TIMESTAMPTZ,
    approval_expires_at TIMESTAMPTZ,
    rejected_reason     VARCHAR(64),
    decided_at          TIMESTAMPTZ,
    created_at          TIMESTAMPTZ   NOT NULL,
    updated_at          TIMESTAMPTZ   NOT NULL,
    version             BIGINT        NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_price_versions PRIMARY KEY (id),
    CONSTRAINT uq_devplat_price_versions_seq UNIQUE (price_book_id, seq),
    CONSTRAINT fk_devplat_price_versions_book FOREIGN KEY (price_book_id) REFERENCES devplat_price_books (id),
    CONSTRAINT ck_devplat_price_versions_seq CHECK (seq >= 1),
    CONSTRAINT ck_devplat_price_versions_state CHECK (state IN ('DRAFT', 'SUBMITTED', 'APPROVED', 'REJECTED', 'SUPERSEDED')),
    CONSTRAINT ck_devplat_price_versions_rates CHECK (jsonb_typeof(rates) = 'object' AND octet_length(rates::text) <= 65536),
    CONSTRAINT ck_devplat_price_versions_digest CHECK (rates_digest ~ '^[0-9a-f]{64}$'
        AND (submitted_digest IS NULL OR submitted_digest ~ '^[0-9a-f]{64}$')),
    CONSTRAINT ck_devplat_price_versions_submitted CHECK (state = 'DRAFT' OR (submitted_at IS NOT NULL AND submitted_digest IS NOT NULL)),
    CONSTRAINT ck_devplat_price_versions_approved CHECK (
        state NOT IN ('APPROVED', 'SUPERSEDED') OR (approved_at IS NOT NULL AND approval_expires_at IS NOT NULL)),
    CONSTRAINT ck_devplat_price_versions_reason CHECK (rejected_reason IS NULL OR rejected_reason ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_price_versions_version CHECK (version >= 1)
);
CREATE INDEX ix_devplat_price_versions_book ON devplat_price_versions (price_book_id, created_at DESC);
-- A version's rates are frozen once it leaves DRAFT; a decided version never returns.
CREATE FUNCTION devplat_price_versions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.price_book_id <> OLD.price_book_id OR NEW.seq <> OLD.seq OR NEW.created_at <> OLD.created_at
       OR (OLD.state <> 'DRAFT' AND (NEW.rates <> OLD.rates OR NEW.rates_digest <> OLD.rates_digest))
       OR (OLD.state <> 'DRAFT' AND NEW.state = 'DRAFT')
       OR (OLD.state = 'REJECTED') THEN
        RAISE EXCEPTION 'devplat_price_versions: a version is frozen once it leaves DRAFT' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_price_versions_guard BEFORE UPDATE ON devplat_price_versions
    FOR EACH ROW EXECUTE FUNCTION devplat_price_versions_guard();
CREATE TRIGGER tr_devplat_price_versions_no_delete BEFORE DELETE ON devplat_price_versions
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_price_activations (
    id              UUID          NOT NULL,
    price_book_id   UUID          NOT NULL,
    version_id      UUID          NOT NULL,
    kind            VARCHAR(16)   NOT NULL,
    effective_from  TIMESTAMPTZ   NOT NULL,
    operator_ref    VARCHAR(160)  NOT NULL,
    approver_ref    VARCHAR(160),
    recorded_at     TIMESTAMPTZ   NOT NULL,
    seq             BIGSERIAL     NOT NULL,
    CONSTRAINT pk_devplat_price_activations PRIMARY KEY (id),
    CONSTRAINT uq_devplat_price_activations_seq UNIQUE (seq),
    CONSTRAINT fk_devplat_price_activations_book FOREIGN KEY (price_book_id) REFERENCES devplat_price_books (id),
    CONSTRAINT fk_devplat_price_activations_version FOREIGN KEY (version_id) REFERENCES devplat_price_versions (id),
    CONSTRAINT ck_devplat_price_activations_kind CHECK (kind IN ('SCHEDULED', 'ACTIVATED', 'ROLLED_BACK'))
);
CREATE INDEX ix_devplat_price_activations_book ON devplat_price_activations (price_book_id, effective_from DESC, seq DESC);
CREATE TRIGGER tr_devplat_price_activations_append_only BEFORE UPDATE OR DELETE ON devplat_price_activations
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_price_activations_no_truncate BEFORE TRUNCATE ON devplat_price_activations
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_market_freezes (
    id            UUID          NOT NULL,
    market_code   VARCHAR(16)   NOT NULL,
    state         VARCHAR(16)   NOT NULL,
    reason_code   VARCHAR(64)   NOT NULL,
    changed_by    VARCHAR(160)  NOT NULL,
    frozen_at     TIMESTAMPTZ,
    created_at    TIMESTAMPTZ   NOT NULL,
    updated_at    TIMESTAMPTZ   NOT NULL,
    version       BIGINT        NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_market_freezes PRIMARY KEY (id),
    CONSTRAINT uq_devplat_market_freezes_market UNIQUE (market_code),
    CONSTRAINT ck_devplat_market_freezes_market CHECK (market_code ~ '^[A-Z][A-Z0-9_]{1,15}$'),
    CONSTRAINT ck_devplat_market_freezes_state CHECK (state IN ('FROZEN', 'UNFROZEN')),
    CONSTRAINT ck_devplat_market_freezes_frozen CHECK ((state = 'FROZEN') = (frozen_at IS NOT NULL)),
    CONSTRAINT ck_devplat_market_freezes_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
);

CREATE TABLE devplat_plan_holders (
    id                  UUID          NOT NULL,
    holder_kind         VARCHAR(16)   NOT NULL,
    holder_ref          UUID          NOT NULL,
    plan_code           VARCHAR(32)   NOT NULL,
    price_book_id       UUID          NOT NULL,
    billing_account_ref VARCHAR(128),
    updated_at          TIMESTAMPTZ   NOT NULL,
    version             BIGINT        NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_plan_holders PRIMARY KEY (id),
    CONSTRAINT uq_devplat_plan_holders_holder UNIQUE (holder_kind, holder_ref),
    CONSTRAINT fk_devplat_plan_holders_book FOREIGN KEY (price_book_id) REFERENCES devplat_price_books (id),
    CONSTRAINT ck_devplat_plan_holders_kind CHECK (holder_kind IN ('USER', 'ORGANIZATION')),
    CONSTRAINT ck_devplat_plan_holders_plan CHECK (plan_code ~ '^[A-Z][A-Z_]{1,23}$'),
    CONSTRAINT ck_devplat_plan_holders_billing CHECK (billing_account_ref IS NULL OR billing_account_ref ~ '^[A-Za-z0-9._:/-]{1,128}$')
);
CREATE TABLE devplat_plan_assignments_consumer (
    id              UUID          NOT NULL,
    holder_id       UUID          NOT NULL,
    holder_kind     VARCHAR(16)   NOT NULL,
    plan_code       VARCHAR(32)   NOT NULL,
    price_book_id   UUID          NOT NULL,
    reason_code     VARCHAR(64)   NOT NULL,
    assigned_by     VARCHAR(160)  NOT NULL,
    effective_from  TIMESTAMPTZ   NOT NULL,
    seq             BIGSERIAL     NOT NULL,
    CONSTRAINT pk_devplat_plan_assignments_consumer PRIMARY KEY (id),
    CONSTRAINT uq_devplat_plan_assignments_consumer_seq UNIQUE (seq),
    CONSTRAINT fk_devplat_plan_assignments_consumer_holder FOREIGN KEY (holder_id) REFERENCES devplat_plan_holders (id),
    CONSTRAINT ck_devplat_plan_assignments_consumer_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
CREATE INDEX ix_devplat_plan_assignments_consumer_holder ON devplat_plan_assignments_consumer (holder_id, seq);
CREATE TRIGGER tr_devplat_plan_assignments_consumer_append_only BEFORE UPDATE OR DELETE ON devplat_plan_assignments_consumer
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_plan_overrides (
    id            UUID          NOT NULL,
    holder_kind   VARCHAR(16)   NOT NULL,
    holder_ref    UUID          NOT NULL,
    state         VARCHAR(16)   NOT NULL,
    limits        JSONB         NOT NULL,
    reason_code   VARCHAR(64)   NOT NULL,
    changed_by    VARCHAR(160)  NOT NULL,
    created_at    TIMESTAMPTZ   NOT NULL,
    updated_at    TIMESTAMPTZ   NOT NULL,
    version       BIGINT        NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_plan_overrides PRIMARY KEY (id),
    CONSTRAINT uq_devplat_plan_overrides_holder UNIQUE (holder_kind, holder_ref),
    CONSTRAINT ck_devplat_plan_overrides_kind CHECK (holder_kind IN ('USER', 'ORGANIZATION')),
    CONSTRAINT ck_devplat_plan_overrides_state CHECK (state IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_devplat_plan_overrides_limits CHECK (jsonb_typeof(limits) = 'object' AND octet_length(limits::text) <= 2048),
    CONSTRAINT ck_devplat_plan_overrides_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_plan_overrides;
DROP TABLE IF EXISTS devplat_plan_assignments_consumer;
DROP TABLE IF EXISTS devplat_plan_holders;
DROP TABLE IF EXISTS devplat_market_freezes;
DROP TABLE IF EXISTS devplat_price_activations;
DROP TABLE IF EXISTS devplat_price_versions;
DROP FUNCTION IF EXISTS devplat_price_versions_guard();
DROP TABLE IF EXISTS devplat_price_books;
ALTER TABLE devplat_admin_operations DROP CONSTRAINT ck_devplat_admin_operations_subject;
ALTER TABLE devplat_admin_operations ADD CONSTRAINT ck_devplat_admin_operations_subject CHECK (subject_kind IN
    ('PRODUCTION_ACCESS', 'APP', 'ORGANIZATION', 'SUPPORT_PROFILE', 'PUSH_KEY', 'WEBHOOK_DELIVERY', 'USAGE_ROLLUPS')) NOT VALID;
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
