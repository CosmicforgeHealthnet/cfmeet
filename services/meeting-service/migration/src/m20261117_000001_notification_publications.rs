//! Notification Service roadmap N7 / N11 — Meeting's publications of its STANDARD
//! categories (`MEETING_INVITATION`, `MEETING_NOTIFICATION`, `ORGANIZATION_INVITATION`) to
//! the Notification Service (blueprint Document 4 §9.3, §9.5, §5.1).
//!
//! Second migration of the SEPARATE additive history `notification_fence_migrations`
//! (binary `notification-fence-migration`): previously deployed images never read these
//! objects and keep starting after it is applied (their `routingProtocol` readiness fact
//! fails closed on the version they do not know, which is what the activation operation
//! certifies). Additive; inert while every category is `LEGACY_ONLY`.
//!
//! * `meeting_notification_publications` — one row per routed notification the
//!   Notification Service must hear about, written in the transaction that records the
//!   routing decision:
//!   - `shadow = TRUE`: the SHADOW copy of an `OLD` decision (the legacy email / job row is
//!     written as well); posted to I1 with `shadow: true`, recorded and never delivered;
//!   - `shadow = FALSE`: the publication of a `NEW` decision (no legacy row).
//!
//!   At most one row per decision and per envelope event id. The envelope
//!   (`cfmeet.notifications.event`) is built and validated once, at enqueue (an invitation
//!   token is never stored by Meeting, so it can only be sealed then), and re-posted
//!   byte-identically until I1 answers. Ids only: the CHECK refuses any data key outside
//!   the catalogue keys of the five Meeting event types and a sealed part on any type but
//!   the two invitation emails (whose token / address are HPKE-sealed to the service key,
//!   which Meeting cannot open). The sealed part is erased when the row settles.
//!   States: `PENDING` → `ACKED` (I1 `ACCEPTED` / `DUPLICATE` / `HELD`), `DEAD` (refused),
//!   `EXPIRED` (never posted in time), `SUPERSEDED` (decision rerouted to `OLD`, §9.5).
//!
//! * The route decision `reason` CHECK gains `RECIPIENT_LIMIT` (the audience does not fit
//!   one envelope: decided `OLD` even when the canary picks `NEW`) and `UNPUBLISHABLE` (the
//!   notification has no complete catalogue envelope — for example an email-only recipient
//!   the service cannot address from an envelope, an invitation without a conference, or
//!   no sealing key configured — so it stays on the legacy path); the decision-row guard
//!   accepts either reason on insert, for an `OLD` decision only.
//!
//! `down` refuses while any category is not `LEGACY_ONLY` or any decision carries one of
//! the new reasons; it keeps the (backward-compatible) guard function, which the restored
//! CHECK makes equivalent to the first migration's.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS meeting_notification_publications (
    id              UUID         PRIMARY KEY,
    event_id        VARCHAR(36)  NOT NULL UNIQUE CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    decision_id     UUID         NOT NULL UNIQUE
                        REFERENCES notification_route_decisions (decision_id) ON DELETE CASCADE,
    routing_version INT          NOT NULL CHECK (routing_version >= 1),
    category        VARCHAR(40)  NOT NULL CHECK (category IN
                        ('MEETING_INVITATION', 'MEETING_NOTIFICATION', 'ORGANIZATION_INVITATION')),
    event_type      VARCHAR(80)  NOT NULL CHECK (event_type IN (
                        'meeting-service.conference.scheduled',
                        'meeting-service.conference.rescheduled',
                        'meeting-service.conference.cancelled',
                        'meeting-service.invitation.email-requested',
                        'meeting-service.organization-invitation.email-requested')),
    shadow          BOOLEAN      NOT NULL,
    envelope        JSONB        NOT NULL,
    state           VARCHAR(10)  NOT NULL DEFAULT 'PENDING'
                        CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'EXPIRED', 'SUPERSEDED')),
    outcome         VARCHAR(9)   CHECK (outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    last_error      VARCHAR(64)  CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    attempts        INT          NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ  NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT ck_meeting_notification_publications_done
        CHECK ((state = 'PENDING') = (completed_at IS NULL)),
    -- Ids only: the catalogue data keys of the five Meeting types; a sealed part only on
    -- the two invitation emails (HPKE to the service key).
    CONSTRAINT ck_meeting_notification_publications_envelope CHECK (
        jsonb_typeof(envelope) = 'object'
        AND octet_length(envelope::text) <= 12288
        AND envelope ->> 'eventId' = event_id
        AND envelope ->> 'eventType' = event_type
        AND jsonb_typeof(envelope -> 'data') = 'object'
        AND (envelope -> 'data') - ARRAY['conferenceId', 'startsAt', 'invitationId', 'organizationId'] = '{}'::jsonb
        AND (NOT (envelope ? 'sealed') OR event_type IN (
                'meeting-service.invitation.email-requested',
                'meeting-service.organization-invitation.email-requested')))
);
CREATE INDEX IF NOT EXISTS ix_meeting_notification_publications_due
    ON meeting_notification_publications (next_attempt_at) WHERE state = 'PENDING';
CREATE INDEX IF NOT EXISTS ix_meeting_notification_publications_completed
    ON meeting_notification_publications (completed_at) WHERE state <> 'PENDING';

ALTER TABLE notification_route_decisions
    DROP CONSTRAINT IF EXISTS notification_route_decisions_reason_check,
    ADD CONSTRAINT notification_route_decisions_reason_check CHECK (reason IN ('ROUTED',
        'REROUTED_UNACKED', 'REROUTED_CANCELLED', 'REROUTED_CRITICAL_LEASE', 'AUTO_FAILOVER',
        'REROUTED_POSSIBLE_DUPLICATE', 'RECIPIENT_LIMIT', 'UNPUBLISHABLE'));

-- F3, as installed by m20261116_000001, except that a new decision may also be recorded
-- `OLD` with reason RECIPIENT_LIMIT / UNPUBLISHABLE (a NEW route the event cannot take).
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
        IF NOT (NEW.reason = 'ROUTED'
                OR (NEW.reason IN ('RECIPIENT_LIMIT', 'UNPUBLISHABLE') AND NEW.route = 'OLD'))
           OR NEW.decision_version <> 1 OR NEW.lease_count <> 0
           OR NEW.ns_acked_at IS NOT NULL OR NEW.send_lease_until IS NOT NULL
           OR NEW.ns_completed_at IS NOT NULL THEN
            RAISE EXCEPTION 'ROUTING_DECISION_IMMUTABLE: a new decision is ROUTED (or OLD by capability), version 1, unacked and unleased'
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
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notification_routing_state WHERE mode <> 'LEGACY_ONLY') THEN
        RAISE EXCEPTION 'refusing to remove the notification publications: a category is not LEGACY_ONLY';
    END IF;
    IF EXISTS (SELECT 1 FROM notification_route_decisions
                WHERE reason IN ('RECIPIENT_LIMIT', 'UNPUBLISHABLE')) THEN
        RAISE EXCEPTION 'refusing to remove the notification publications: decisions record capability reasons';
    END IF;
END
$$;
DROP TABLE IF EXISTS meeting_notification_publications;
ALTER TABLE notification_route_decisions
    DROP CONSTRAINT IF EXISTS notification_route_decisions_reason_check,
    ADD CONSTRAINT notification_route_decisions_reason_check CHECK (reason IN ('ROUTED',
        'REROUTED_UNACKED', 'REROUTED_CANCELLED', 'REROUTED_CRITICAL_LEASE', 'AUTO_FAILOVER',
        'REROUTED_POSSIBLE_DUPLICATE'));
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
