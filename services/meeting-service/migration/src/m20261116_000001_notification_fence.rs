//! Notification Service roadmap N7 — routing fence protocol v1 foundation (blueprint
//! Document 4 §9) in the Meeting database.
//!
//! Part of the SEPARATE additive history `notification_fence_migrations` (binary
//! `notification-fence-migration`): the main migrator fails closed on unknown versions, so
//! previously deployed images keep starting after this history is applied (rollback
//! safety, DATA-011 target rule). The fence-capable image reports `routingProtocol: 1` in
//! `/health` only once this history is fully applied; nothing requires it at startup.
//!
//! Additive only; no existing column or constraint changes:
//!
//! * `notification_routing_state` — the runtime-readable routing switch per category
//!   (§9.2), seeded `LEGACY_ONLY` for every Meeting-owned routing category (below).
//! * `notification_route_decisions` — one decision per routable message (F1), keyed by its
//!   deterministic idempotency key. A guard trigger (F3) refuses every change except the
//!   audited transitions of §9.2/§9.8 (reroute NEW → OLD with `decision_version + 1` and a
//!   `REROUTED_*` / `AUTO_FAILOVER` reason, the one-time acknowledgement, CRITICAL lease
//!   grants and completion). A completed decision is terminal; a decision is deleted only
//!   after `expires_at`.
//! * `account_notification_outbox` — the CRITICAL lane outbox (sealed payloads, §9.2).
//! * Legacy guard triggers (§9.4) `BEFORE INSERT` on `email_deliveries` (key
//!   `idempotency_key`) and `meeting_notifications` (key `dedupe_key`). In `LEGACY_ONLY`
//!   (and for rows that map to no routing category, or a category without a state row)
//!   they are inert. Otherwise the insert requires a decision with `route = 'OLD'` for the
//!   key, or raises `ROUTING_DECISION_REQUIRED` with SQLSTATE `CFN01`.
//!
//! Meeting-owned routing categories (the Notification Service `notif_routing` seed of
//! `services/notifications` m20261116_000001 and its `catalogue.rs`, restricted to the
//! event types whose producer is `cfmeet.meeting-service` in
//! `packages/notification-client` / `packages/contracts/notifications`):
//!
//! | routing category            | catalogue event type                                      | lane     |
//! |-----------------------------|-----------------------------------------------------------|----------|
//! | `AUTH_VERIFICATION`         | `account.verification-code.requested`                     | CRITICAL |
//! | `AUTH_PASSWORD_RESET`       | `account.password-reset-code.requested`                   | CRITICAL |
//! | `SECURITY_DEVICE_STEP_UP`   | `account.device-step-up-code.requested`                   | CRITICAL |
//! | `SECURITY_PASSWORD_CHANGED` | `account.security.password-changed`                       | CRITICAL |
//! | `SECURITY_DEVICE_LINKED`    | `account.security.device-linked`                          | CRITICAL |
//! | `MEETING_INVITATION`        | `meeting-service.invitation.email-requested`              | STANDARD |
//! | `MEETING_NOTIFICATION`      | `meeting-service.conference.{scheduled,rescheduled,cancelled}` | STANDARD |
//! | `ORGANIZATION_INVITATION`   | `meeting-service.organization-invitation.email-requested` | STANDARD |
//!
//! Old-path row → routing category (SQL functions below):
//!
//! * `email_deliveries.category` (the ledger category written by `email_delivery::enqueue`):
//!   `AUTH_VERIFICATION`, `AUTH_PASSWORD_RESET`, `SECURITY_DEVICE_STEP_UP`,
//!   `SECURITY_PASSWORD_CHANGED`, `MEETING_INVITATION`, `MEETING_NOTIFICATION`,
//!   `ORGANIZATION_INVITATION` map to the category of the same name; `NOTIFICATION` with a
//!   `device-linked:` key maps to `SECURITY_DEVICE_LINKED` (today's ledger records device
//!   notices under `NOTIFICATION`); `DEVELOPER_PLATFORM` maps to `DEVELOPER_PLATFORM`,
//!   which has no Meeting state row (Developer Platform is the producer of record), so it
//!   stays inert unless an operator adds one. Every other category (`AUTH_WELCOME`, other
//!   `NOTIFICATION` notices, `OPERATIONS_ACCEPTANCE`, `LEGACY_QUEUE`) maps to none and is
//!   never fenced.
//! * `meeting_notifications.kind`: `CONFERENCE_RESCHEDULED`, `CONFERENCE_CANCELLED`,
//!   `CONFERENCE_REMINDER` all map to `MEETING_NOTIFICATION`; the decision key is the job's
//!   `dedupe_key` (`rescheduled:…`, `cancelled:…`, `reminder:<conference>:<start>`, §9.9).
//! * A `MEETING_NOTIFICATION` email whose key is `meeting-notification:<job id>:<recipient>`
//!   is the per-recipient fan-out of an existing job: the job's decision covers it (as the
//!   push decision covers every device), so it is allowed when the job exists and its
//!   decision is not `NEW`. Any other key needs its own `OLD` decision.
//!
//! `down` refuses while any category is not `LEGACY_ONLY` (the database half of the
//! rollback floor, §9.6).
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS notification_routing_state (
    category          VARCHAR(40) PRIMARY KEY,
    mode              VARCHAR(12) NOT NULL
                          CHECK (mode IN ('LEGACY_ONLY', 'SHADOW', 'FENCED_OLD', 'FENCED_NEW')),
    canary_percent    SMALLINT    NOT NULL DEFAULT 0 CHECK (canary_percent BETWEEN 0 AND 100),
    critical_fallback VARCHAR(8)  NOT NULL DEFAULT 'AUTO'
                          CHECK (critical_fallback IN ('AUTO', 'FORCED', 'OFF')),
    routing_version   INT         NOT NULL,
    protocol          SMALLINT    NOT NULL DEFAULT 1,
    changed_by_run    VARCHAR(64) NOT NULL,
    changed_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS notification_route_decisions (
    idempotency_key  VARCHAR(160) PRIMARY KEY,
    decision_id      UUID         NOT NULL UNIQUE,
    category         VARCHAR(40)  NOT NULL,
    route            VARCHAR(4)   NOT NULL CHECK (route IN ('OLD', 'NEW')),
    decision_version INT          NOT NULL DEFAULT 1,
    routing_version  INT          NOT NULL,
    reason           VARCHAR(32)  NOT NULL CHECK (reason IN ('ROUTED', 'REROUTED_UNACKED',
                         'REROUTED_CANCELLED', 'REROUTED_CRITICAL_LEASE', 'AUTO_FAILOVER',
                         'REROUTED_POSSIBLE_DUPLICATE')),
    ns_acked_at      TIMESTAMPTZ,
    send_lease_until TIMESTAMPTZ,
    lease_count      INT          NOT NULL DEFAULT 0,
    ns_completed_at  TIMESTAMPTZ,
    decided_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ  NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_notification_route_decisions_expiry
    ON notification_route_decisions (expires_at);
CREATE INDEX IF NOT EXISTS ix_notification_route_decisions_unacked
    ON notification_route_decisions (category, decided_at)
    WHERE route = 'NEW' AND ns_acked_at IS NULL;

CREATE TABLE IF NOT EXISTS account_notification_outbox (
    id              UUID PRIMARY KEY,
    decision_id     UUID        NOT NULL UNIQUE
                        REFERENCES notification_route_decisions (decision_id),
    event           JSONB       NOT NULL,
    fallback_sealed BYTEA       NOT NULL,
    state           VARCHAR(12) NOT NULL
                        CHECK (state IN ('PENDING', 'ACKED', 'SUPERSEDED', 'EXPIRED')),
    attempts        INT         NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_id        UUID,
    lease_until     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_account_notification_outbox_due
    ON account_notification_outbox (next_attempt_at) WHERE state = 'PENDING';

-- Every Meeting-owned routing category starts inert (§9.4).
INSERT INTO notification_routing_state (category, mode, routing_version, changed_by_run) VALUES
    ('AUTH_VERIFICATION',         'LEGACY_ONLY', 1, 'migration'),
    ('AUTH_PASSWORD_RESET',       'LEGACY_ONLY', 1, 'migration'),
    ('SECURITY_DEVICE_STEP_UP',   'LEGACY_ONLY', 1, 'migration'),
    ('SECURITY_PASSWORD_CHANGED', 'LEGACY_ONLY', 1, 'migration'),
    ('SECURITY_DEVICE_LINKED',    'LEGACY_ONLY', 1, 'migration'),
    ('MEETING_INVITATION',        'LEGACY_ONLY', 1, 'migration'),
    ('MEETING_NOTIFICATION',      'LEGACY_ONLY', 1, 'migration'),
    ('ORGANIZATION_INVITATION',   'LEGACY_ONLY', 1, 'migration')
ON CONFLICT (category) DO NOTHING;

-- F3: a decision never changes silently.
CREATE OR REPLACE FUNCTION notification_route_decisions_guard() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.expires_at > now() THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a decision is deleted only after it expires'
                USING ERRCODE = 'CFN02';
        END IF;
        RETURN OLD;
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.reason <> 'ROUTED' OR NEW.decision_version <> 1 OR NEW.lease_count <> 0
           OR NEW.ns_acked_at IS NOT NULL OR NEW.send_lease_until IS NOT NULL
           OR NEW.ns_completed_at IS NOT NULL THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a new decision is ROUTED, version 1, unacked and unleased'
                USING ERRCODE = 'CFN02';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
       OR NEW.decision_id IS DISTINCT FROM OLD.decision_id
       OR NEW.category IS DISTINCT FROM OLD.category
       OR NEW.routing_version IS DISTINCT FROM OLD.routing_version
       OR NEW.decided_at IS DISTINCT FROM OLD.decided_at
       OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: decision identity is immutable'
            USING ERRCODE = 'CFN02';
    END IF;
    -- Reroute: NEW -> OLD only, audited, never for a completed decision or a live lease.
    IF NEW.route IS DISTINCT FROM OLD.route THEN
        IF OLD.ns_completed_at IS NOT NULL THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a completed decision is terminal'
                USING ERRCODE = 'CFN02';
        END IF;
        IF NOT (OLD.route = 'NEW' AND NEW.route = 'OLD') THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: only NEW -> OLD reroutes exist'
                USING ERRCODE = 'CFN02';
        END IF;
        IF NEW.decision_version <> OLD.decision_version + 1 THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a reroute increments decision_version'
                USING ERRCODE = 'CFN02';
        END IF;
        IF NEW.reason NOT IN ('REROUTED_UNACKED', 'REROUTED_CANCELLED', 'REROUTED_CRITICAL_LEASE',
                              'AUTO_FAILOVER', 'REROUTED_POSSIBLE_DUPLICATE') THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a reroute records its reason'
                USING ERRCODE = 'CFN02';
        END IF;
        IF OLD.send_lease_until IS NOT NULL AND OLD.send_lease_until > clock_timestamp() THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a live send lease blocks the reroute'
                USING ERRCODE = 'CFN02';
        END IF;
        IF NEW.reason = 'REROUTED_UNACKED' AND (OLD.ns_acked_at IS NOT NULL OR OLD.lease_count <> 0) THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: only an unacknowledged decision is rerouted unilaterally'
                USING ERRCODE = 'CFN02';
        END IF;
        IF NEW.reason IN ('REROUTED_CRITICAL_LEASE', 'AUTO_FAILOVER') AND OLD.lease_count <> 0 THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a leased decision is rerouted only as a possible duplicate'
                USING ERRCODE = 'CFN02';
        END IF;
        IF NEW.reason = 'REROUTED_POSSIBLE_DUPLICATE' AND OLD.lease_count = 0 THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a never-leased decision is not a possible duplicate'
                USING ERRCODE = 'CFN02';
        END IF;
    ELSIF NEW.decision_version IS DISTINCT FROM OLD.decision_version
          OR NEW.reason IS DISTINCT FROM OLD.reason THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: version and reason change only with a reroute'
            USING ERRCODE = 'CFN02';
    END IF;
    -- Acknowledgement (F5): set once, for a NEW decision.
    IF OLD.ns_acked_at IS NOT NULL AND NEW.ns_acked_at IS DISTINCT FROM OLD.ns_acked_at THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: the acknowledgement is recorded once'
            USING ERRCODE = 'CFN02';
    END IF;
    IF OLD.ns_acked_at IS NULL AND NEW.ns_acked_at IS NOT NULL AND OLD.route <> 'NEW' THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: only a NEW decision is acknowledged'
            USING ERRCODE = 'CFN02';
    END IF;
    -- CRITICAL lease (X6): granted one at a time on an uncompleted NEW decision.
    IF NEW.lease_count IS DISTINCT FROM OLD.lease_count THEN
        IF NEW.lease_count <> OLD.lease_count + 1 OR OLD.route <> 'NEW' OR NEW.route <> 'NEW'
           OR OLD.ns_completed_at IS NOT NULL OR NEW.send_lease_until IS NULL THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a lease is granted once per claim on an uncompleted NEW decision'
                USING ERRCODE = 'CFN02';
        END IF;
    ELSIF NEW.send_lease_until IS DISTINCT FROM OLD.send_lease_until THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: the send lease changes only with a grant'
            USING ERRCODE = 'CFN02';
    END IF;
    -- Completion (X7): terminal, only after a granted lease on a NEW decision.
    IF OLD.ns_completed_at IS NOT NULL AND NEW.ns_completed_at IS DISTINCT FROM OLD.ns_completed_at THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a completed decision is terminal'
            USING ERRCODE = 'CFN02';
    END IF;
    IF OLD.ns_completed_at IS NULL AND NEW.ns_completed_at IS NOT NULL
       AND (NEW.route <> 'NEW' OR NEW.lease_count = 0) THEN
        RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: completion requires a granted lease on a NEW decision'
            USING ERRCODE = 'CFN02';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_notification_route_decisions_guard ON notification_route_decisions;
CREATE TRIGGER trg_notification_route_decisions_guard
    BEFORE INSERT OR UPDATE OR DELETE ON notification_route_decisions
    FOR EACH ROW EXECUTE FUNCTION notification_route_decisions_guard();

-- Old-path row -> routing category (NULL: never fenced). See the module documentation.
CREATE OR REPLACE FUNCTION notification_fence_email_category(p_category TEXT, p_key TEXT)
RETURNS TEXT AS $$
    SELECT CASE p_category
        WHEN 'AUTH_VERIFICATION'         THEN 'AUTH_VERIFICATION'
        WHEN 'AUTH_PASSWORD_RESET'       THEN 'AUTH_PASSWORD_RESET'
        WHEN 'SECURITY_DEVICE_STEP_UP'   THEN 'SECURITY_DEVICE_STEP_UP'
        WHEN 'SECURITY_PASSWORD_CHANGED' THEN 'SECURITY_PASSWORD_CHANGED'
        WHEN 'MEETING_INVITATION'        THEN 'MEETING_INVITATION'
        WHEN 'MEETING_NOTIFICATION'      THEN 'MEETING_NOTIFICATION'
        WHEN 'ORGANIZATION_INVITATION'   THEN 'ORGANIZATION_INVITATION'
        WHEN 'DEVELOPER_PLATFORM'        THEN 'DEVELOPER_PLATFORM'
        WHEN 'NOTIFICATION' THEN
            CASE WHEN p_key LIKE 'device-linked:%' THEN 'SECURITY_DEVICE_LINKED' END
    END
$$ LANGUAGE sql IMMUTABLE;

CREATE OR REPLACE FUNCTION notification_fence_meeting_notification_category(p_kind TEXT)
RETURNS TEXT AS $$
    SELECT CASE p_kind
        WHEN 'CONFERENCE_RESCHEDULED' THEN 'MEETING_NOTIFICATION'
        WHEN 'CONFERENCE_CANCELLED'   THEN 'MEETING_NOTIFICATION'
        WHEN 'CONFERENCE_REMINDER'    THEN 'MEETING_NOTIFICATION'
    END
$$ LANGUAGE sql IMMUTABLE;

-- §9.4 rule: inert in LEGACY_ONLY (or without a category / state row); otherwise an OLD
-- decision must exist for the key.
CREATE OR REPLACE FUNCTION notification_fence_require_old(p_category TEXT, p_key TEXT)
RETURNS VOID AS $$
DECLARE
    v_mode TEXT;
BEGIN
    IF p_category IS NULL THEN
        RETURN;
    END IF;
    SELECT mode INTO v_mode FROM notification_routing_state WHERE category = p_category;
    IF NOT FOUND OR v_mode = 'LEGACY_ONLY' THEN
        RETURN;
    END IF;
    IF EXISTS (SELECT 1 FROM notification_route_decisions
                WHERE idempotency_key = p_key AND route = 'OLD') THEN
        RETURN;
    END IF;
    RAISE EXCEPTION 'ROUTING_DECISION_REQUIRED'
        USING ERRCODE = 'CFN01',
              DETAIL = 'category ' || p_category || ' is ' || v_mode
                       || ': an old-path row needs a routing decision with route OLD';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION email_deliveries_routing_guard() RETURNS trigger AS $$
DECLARE
    v_category TEXT := notification_fence_email_category(NEW.category, NEW.idempotency_key);
    v_job_key  TEXT;
BEGIN
    IF v_category IS NULL THEN
        RETURN NEW;
    END IF;
    -- Per-recipient fan-out of a meeting notification job: the job's decision covers it.
    IF v_category = 'MEETING_NOTIFICATION'
       AND NEW.idempotency_key ~
           '^meeting-notification:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}:' THEN
        SELECT n.dedupe_key INTO v_job_key FROM meeting_notifications n
         WHERE n.id = substring(NEW.idempotency_key FROM 22 FOR 36)::uuid;
        IF FOUND THEN
            IF EXISTS (SELECT 1 FROM notification_route_decisions
                        WHERE idempotency_key = v_job_key AND route = 'NEW') THEN
                RAISE EXCEPTION 'ROUTING_DECISION_REQUIRED'
                    USING ERRCODE = 'CFN01',
                          DETAIL = 'the meeting notification job is routed NEW';
            END IF;
            RETURN NEW;
        END IF;
    END IF;
    PERFORM notification_fence_require_old(v_category, NEW.idempotency_key);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_email_deliveries_routing_guard ON email_deliveries;
CREATE TRIGGER trg_email_deliveries_routing_guard
    BEFORE INSERT ON email_deliveries
    FOR EACH ROW EXECUTE FUNCTION email_deliveries_routing_guard();

CREATE OR REPLACE FUNCTION meeting_notifications_routing_guard() RETURNS trigger AS $$
BEGIN
    PERFORM notification_fence_require_old(
        notification_fence_meeting_notification_category(NEW.kind), NEW.dedupe_key);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_meeting_notifications_routing_guard ON meeting_notifications;
CREATE TRIGGER trg_meeting_notifications_routing_guard
    BEFORE INSERT ON meeting_notifications
    FOR EACH ROW EXECUTE FUNCTION meeting_notifications_routing_guard();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notification_routing_state WHERE mode <> 'LEGACY_ONLY') THEN
        RAISE EXCEPTION 'refusing to remove the routing fence: a category is not LEGACY_ONLY';
    END IF;
END
$$;
DROP TRIGGER IF EXISTS trg_meeting_notifications_routing_guard ON meeting_notifications;
DROP TRIGGER IF EXISTS trg_email_deliveries_routing_guard ON email_deliveries;
DROP FUNCTION IF EXISTS meeting_notifications_routing_guard();
DROP FUNCTION IF EXISTS email_deliveries_routing_guard();
DROP FUNCTION IF EXISTS notification_fence_require_old(TEXT, TEXT);
DROP FUNCTION IF EXISTS notification_fence_meeting_notification_category(TEXT);
DROP FUNCTION IF EXISTS notification_fence_email_category(TEXT, TEXT);
DROP TABLE IF EXISTS account_notification_outbox;
DROP TABLE IF EXISTS notification_route_decisions;
DROP FUNCTION IF EXISTS notification_route_decisions_guard();
DROP TABLE IF EXISTS notification_routing_state;
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
