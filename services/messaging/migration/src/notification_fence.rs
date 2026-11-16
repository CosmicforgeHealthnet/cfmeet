//! Routing fence protocol v1 (Notification Service specification, Document 4 §9): the
//! producer-side routing switch and decision ledger of Messaging, and the guard trigger
//! on its legacy push path.
//!
//! Independently versioned ([`crate::NotificationFenceMigrator`], history table
//! `messaging_notification_fence_seaql_migrations`) so that a previous Messaging image —
//! whose main migrator fails closed on migrations it does not know — still boots against
//! a database where this history is applied.
//!
//! * `messaging_notification_routing_state` — one row per routing category Messaging
//!   produces (§9.2). Every category starts `LEGACY_ONLY`.
//! * `messaging_notification_route_decisions` — one row per routable message, keyed by its
//!   deterministic idempotency key (F1). For push wake-ups the key is the Messaging outbox
//!   event id in its envelope form (`evt_<32 hex>`, §9.3); one decision covers every
//!   device fan-out of that event. Updates are restricted to the audited reroute (F3).
//! * `trg_messaging_push_jobs_routing_guard` — `BEFORE INSERT` on `messaging_push_jobs`
//!   (§9.4): in `LEGACY_ONLY` (or without a state row) it allows the insert, so pre-fence
//!   binaries behave exactly as before; otherwise it requires a decision with route `OLD`
//!   for the job's event and raises `ROUTING_DECISION_REQUIRED` (F2, F6).
//!
//! The specification names the tables `notification_routing_state` /
//! `notification_route_decisions`; in Messaging they carry the `messaging_` prefix every
//! Messaging table must have (repository guard, `tests/architecture.rs`). Additive and
//! forward-only.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261204_000001_notification_routing_fence_v1"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "the notification routing fence is additive and forward-only".into(),
        ))
    }
}

const UP: &str = r#"
-- Runtime-readable routing switch (no restart). Changed only by routing operations.
CREATE TABLE messaging_notification_routing_state (
    category          VARCHAR(40)  NOT NULL,
    mode              VARCHAR(12)  NOT NULL,
    canary_percent    SMALLINT     NOT NULL DEFAULT 0,
    critical_fallback VARCHAR(8)   NOT NULL DEFAULT 'AUTO',
    routing_version   INTEGER      NOT NULL,
    protocol          SMALLINT     NOT NULL DEFAULT 1,
    changed_by_run    VARCHAR(64)  NOT NULL,
    changed_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_messaging_notification_routing_state PRIMARY KEY (category),
    CONSTRAINT ck_messaging_notification_routing_state_category CHECK (category ~ '^[A-Z][A-Z0-9_]{1,39}$'),
    CONSTRAINT ck_messaging_notification_routing_state_mode CHECK (mode IN ('LEGACY_ONLY', 'SHADOW', 'FENCED_OLD', 'FENCED_NEW')),
    CONSTRAINT ck_messaging_notification_routing_state_canary CHECK (canary_percent BETWEEN 0 AND 100),
    CONSTRAINT ck_messaging_notification_routing_state_fallback CHECK (critical_fallback IN ('AUTO', 'FORCED', 'OFF')),
    CONSTRAINT ck_messaging_notification_routing_state_version CHECK (routing_version >= 1),
    CONSTRAINT ck_messaging_notification_routing_state_protocol CHECK (protocol >= 1),
    CONSTRAINT ck_messaging_notification_routing_state_run CHECK (length(changed_by_run) BETWEEN 1 AND 64 AND changed_by_run !~ '[[:cntrl:]]')
);

-- One row per routable message (F1).
CREATE TABLE messaging_notification_route_decisions (
    idempotency_key  VARCHAR(160) NOT NULL,
    decision_id      UUID         NOT NULL,
    category         VARCHAR(40)  NOT NULL,
    route            VARCHAR(4)   NOT NULL,
    decision_version INTEGER      NOT NULL DEFAULT 1,
    routing_version  INTEGER      NOT NULL,
    reason           VARCHAR(32)  NOT NULL,
    ns_acked_at      TIMESTAMPTZ,
    send_lease_until TIMESTAMPTZ,
    lease_count      INTEGER      NOT NULL DEFAULT 0,
    ns_completed_at  TIMESTAMPTZ,
    decided_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_notification_route_decisions PRIMARY KEY (idempotency_key),
    CONSTRAINT uq_messaging_notification_route_decisions_decision UNIQUE (decision_id),
    CONSTRAINT ck_messaging_notification_route_decisions_key CHECK (length(idempotency_key) BETWEEN 1 AND 160 AND idempotency_key !~ '[[:cntrl:][:space:]]'),
    CONSTRAINT ck_messaging_notification_route_decisions_route CHECK (route IN ('OLD', 'NEW')),
    CONSTRAINT ck_messaging_notification_route_decisions_reason CHECK (reason IN ('ROUTED', 'REROUTED_UNACKED', 'REROUTED_CANCELLED',
        'REROUTED_CRITICAL_LEASE', 'AUTO_FAILOVER', 'REROUTED_POSSIBLE_DUPLICATE')),
    CONSTRAINT ck_messaging_notification_route_decisions_version CHECK (decision_version >= 1 AND routing_version >= 1),
    CONSTRAINT ck_messaging_notification_route_decisions_leases CHECK (lease_count >= 0)
);
CREATE INDEX ix_messaging_notification_route_decisions_expiry ON messaging_notification_route_decisions (expires_at);

-- F3: a decision never changes silently. Allowed updates: the audited reroute (route NEW
-- -> OLD with decision_version + 1 and a REROUTED_* / AUTO_FAILOVER reason), recording the
-- acknowledgement and the completion (once each), the CRITICAL send lease and its count
-- (+1 per lease). A completed decision (ns_completed_at set) is never rerouted.
CREATE FUNCTION messaging_notification_route_decisions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
       OR NEW.decision_id IS DISTINCT FROM OLD.decision_id
       OR NEW.category IS DISTINCT FROM OLD.category
       OR NEW.routing_version IS DISTINCT FROM OLD.routing_version
       OR NEW.decided_at IS DISTINCT FROM OLD.decided_at
       OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE' USING ERRCODE = 'P0001',
            DETAIL = 'the identity of a routing decision never changes';
    END IF;
    IF NEW.route IS DISTINCT FROM OLD.route
       OR NEW.decision_version IS DISTINCT FROM OLD.decision_version
       OR NEW.reason IS DISTINCT FROM OLD.reason THEN
        IF OLD.ns_completed_at IS NOT NULL THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE' USING ERRCODE = 'P0001',
                DETAIL = 'a completed decision is never rerouted';
        END IF;
        IF NOT (OLD.route = 'NEW' AND NEW.route = 'OLD'
                AND NEW.decision_version = OLD.decision_version + 1
                AND NEW.reason IN ('REROUTED_UNACKED', 'REROUTED_CANCELLED', 'REROUTED_CRITICAL_LEASE',
                                   'AUTO_FAILOVER', 'REROUTED_POSSIBLE_DUPLICATE')) THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE' USING ERRCODE = 'P0001',
                DETAIL = 'only an audited reroute (NEW to OLD, decision_version + 1, REROUTED_* or AUTO_FAILOVER) changes a route';
        END IF;
    END IF;
    IF OLD.ns_acked_at IS NOT NULL AND NEW.ns_acked_at IS DISTINCT FROM OLD.ns_acked_at THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE' USING ERRCODE = 'P0001',
            DETAIL = 'the acknowledgement is recorded once';
    END IF;
    IF OLD.ns_completed_at IS NOT NULL AND NEW.ns_completed_at IS DISTINCT FROM OLD.ns_completed_at THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE' USING ERRCODE = 'P0001',
            DETAIL = 'completion is terminal';
    END IF;
    IF NEW.lease_count <> OLD.lease_count AND NEW.lease_count <> OLD.lease_count + 1 THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE' USING ERRCODE = 'P0001',
            DETAIL = 'lease_count grows by one per granted lease';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_messaging_notification_route_decisions_guard BEFORE UPDATE ON messaging_notification_route_decisions
    FOR EACH ROW EXECUTE FUNCTION messaging_notification_route_decisions_guard();

-- §9.4 legacy-table guard: every messaging_push_jobs row is a MESSAGE_WAKEUPS wake-up of
-- the outbox event `event_id`; its routing key is that event's envelope id. A no-op in
-- LEGACY_ONLY (pre-fence behaviour, old binaries unchanged).
CREATE FUNCTION messaging_push_jobs_routing_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    current_mode VARCHAR(12);
BEGIN
    SELECT mode INTO current_mode FROM messaging_notification_routing_state WHERE category = 'MESSAGE_WAKEUPS';
    IF current_mode IS NULL OR current_mode = 'LEGACY_ONLY' THEN
        RETURN NEW;
    END IF;
    IF EXISTS (SELECT 1 FROM messaging_notification_route_decisions
                WHERE idempotency_key = 'evt_' || replace(NEW.event_id::text, '-', '')
                  AND category = 'MESSAGE_WAKEUPS' AND route = 'OLD') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'ROUTING_DECISION_REQUIRED' USING ERRCODE = 'P0001',
        DETAIL = 'MESSAGE_WAKEUPS is not LEGACY_ONLY and the event has no OLD routing decision';
END;
$$;
CREATE TRIGGER trg_messaging_push_jobs_routing_guard BEFORE INSERT ON messaging_push_jobs
    FOR EACH ROW EXECUTE FUNCTION messaging_push_jobs_routing_guard();

-- Every category Messaging produces starts LEGACY_ONLY (dark deployment). Messaging's
-- only legacy delivery path today is the message wake-up push (messaging_push_jobs).
INSERT INTO messaging_notification_routing_state (category, mode, routing_version, changed_by_run) VALUES
    ('MESSAGE_WAKEUPS', 'LEGACY_ONLY', 1, 'migration');
"#;
