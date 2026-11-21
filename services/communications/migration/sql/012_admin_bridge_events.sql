-- ADR 0068 phase A7 (owner events to the HQ admin bridge): the Hub's transactional outbox
-- of `abuse.report.changed.v1` events for profile reports. Additive only: the previous
-- image never names this table and checks its own histories.
--
-- * one row per (report, version), written in the SAME transaction as the report change
--   (created: OPEN at version 1; reviewed by the operator console or the bridge:
--   DISMISSED / ACTIONED at version 2), and only while CF_ADMIN_BRIDGE_BASE_URL is
--   configured (unset: nothing is written or sent);
-- * the event id is derived from (type, report, version): a replayed write never adds a
--   second row and a re-post is byte-identical (the bridge dedupes on producer + event id);
-- * ids, states, versions and times only: never a reporter, target, reason or anything
--   else about the people involved;
-- * the relay leases due rows (lease_id / lease_until, FOR UPDATE SKIP LOCKED) so several
--   replicas never post the same row at once; 202 from the bridge acknowledges the row
--   (ACKED, with the bridge's outcome), a contract refusal (400 / 413) dead-letters it
--   (DEAD), anything else retries with backoff until the row expires (EXPIRED).
--   Settled rows are purged after 30 days.
-- * no foreign key to the report: the report's own 365-day retention may delete it while
--   its event is still waiting.
CREATE TABLE IF NOT EXISTS communication_admin_bridge_events (
 event_id UUID PRIMARY KEY,
 event_type VARCHAR(64) NOT NULL CHECK (event_type IN ('abuse.report.changed.v1')),
 subject_kind VARCHAR(24) NOT NULL CHECK (subject_kind IN ('abuse_report')),
 subject_id UUID NOT NULL,
 subject_state VARCHAR(24) NOT NULL CHECK (subject_state IN ('OPEN','DISMISSED','ACTIONED')),
 subject_version BIGINT NOT NULL CHECK (subject_version >= 1),
 occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 delivery VARCHAR(8) NOT NULL DEFAULT 'PENDING' CHECK (delivery IN ('PENDING','ACKED','DEAD','EXPIRED')),
 outcome VARCHAR(12) CHECK (outcome IS NULL OR outcome IN ('APPENDED','DUPLICATE','STALE','QUARANTINED')),
 last_error VARCHAR(64) CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
 next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 lease_id UUID,
 lease_until TIMESTAMPTZ,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 completed_at TIMESTAMPTZ,
 expires_at TIMESTAMPTZ NOT NULL,
 CONSTRAINT communication_admin_bridge_events_subject UNIQUE (event_type, subject_id, subject_version),
 CHECK ((delivery = 'PENDING') = (completed_at IS NULL)),
 CHECK ((lease_id IS NULL) = (lease_until IS NULL)),
 CHECK (delivery = 'PENDING' OR lease_id IS NULL),
 -- A report is OPEN only at version 1 and reviewed only at version 2.
 CHECK ((subject_state = 'OPEN') = (subject_version = 1))
);
CREATE INDEX IF NOT EXISTS communication_admin_bridge_events_due
 ON communication_admin_bridge_events (next_attempt_at) WHERE delivery = 'PENDING';
CREATE INDEX IF NOT EXISTS communication_admin_bridge_events_retention
 ON communication_admin_bridge_events (completed_at) WHERE delivery <> 'PENDING';
