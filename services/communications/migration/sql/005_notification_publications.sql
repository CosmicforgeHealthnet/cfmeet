-- Notification Service roadmap N7 (Document 4 §5.1, §9.5): the Hub's producer outbox of
-- publications to the Notification Service's I1. Additive only.
--
-- One row per contact-request notification the Hub publishes, written in the SAME
-- transaction as the relationship change, and only while CF_NOTIFICATIONS_BASE_URL is
-- configured:
-- * communications.connection.requested -> the addressee of a new contact request;
-- * communications.connection.connected -> the requester, when the addressee accepts.
-- The relay posts the stored `cfmeet.notifications.event` envelope (built once, ids only:
-- the connection id and the actor's CF Meet account reference; never names, public ids,
-- privacy values or anything else) byte-identically until I1 answers.
-- ACCEPTED / DUPLICATE / HELD acknowledge the row (ACKED); REJECTED or a contract refusal
-- of the batch dead-letters it (DEAD); a principal without a CF Meet account, a blocked
-- pair or a request / connection that no longer holds is SKIPPED with its reason; a
-- row past its expiry is EXPIRED. Settled rows are purged after 30 days.
CREATE TABLE IF NOT EXISTS communication_notification_publications (
 id UUID PRIMARY KEY,
 event_id VARCHAR(36) NOT NULL,
 event_type VARCHAR(64) NOT NULL,
 tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 connection_id UUID NOT NULL REFERENCES communication_connections(id),
 request_id UUID NOT NULL REFERENCES communication_contact_requests(id),
 actor_profile_id UUID NOT NULL, recipient_profile_id UUID NOT NULL,
 envelope JSONB,
 state VARCHAR(8) NOT NULL DEFAULT 'PENDING',
 outcome VARCHAR(9),
 last_error VARCHAR(64),
 attempts INTEGER NOT NULL DEFAULT 0,
 next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 completed_at TIMESTAMPTZ,
 expires_at TIMESTAMPTZ NOT NULL,
 CONSTRAINT communication_notification_publications_event UNIQUE (event_id),
 -- One publication per request and type: replays and retries never write a second row.
 CONSTRAINT communication_notification_publications_request UNIQUE (request_id, event_type),
 -- Tenant / environment isolation: both people are profiles of the row's own scope.
 FOREIGN KEY (tenant_id,application_id,environment_id,actor_profile_id)
  REFERENCES communication_profiles(tenant_id,application_id,environment_id,id),
 FOREIGN KEY (tenant_id,application_id,environment_id,recipient_profile_id)
  REFERENCES communication_profiles(tenant_id,application_id,environment_id,id),
 CHECK (actor_profile_id <> recipient_profile_id),
 CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
 CHECK (event_type IN ('communications.connection.requested','communications.connection.connected')),
 CHECK (state IN ('PENDING','ACKED','DEAD','SKIPPED','EXPIRED')),
 CHECK (outcome IS NULL OR outcome IN ('ACCEPTED','DUPLICATE','HELD','REJECTED')),
 CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
 CHECK (attempts >= 0),
 CHECK ((state = 'PENDING') = (completed_at IS NULL)),
 -- A row waiting for the relay always holds the envelope it will post.
 CHECK (state <> 'PENDING' OR envelope IS NOT NULL),
 -- Ids only: exactly the catalogue's data keys of both types, never a sealed part.
 CHECK (envelope IS NULL OR (
  jsonb_typeof(envelope) = 'object'
  AND octet_length(envelope::text) <= 4096
  AND NOT (envelope ? 'sealed')
  AND jsonb_typeof(envelope -> 'data') = 'object'
  AND (envelope -> 'data') - ARRAY['actorRef','connectionId'] = '{}'::jsonb
  AND envelope ->> 'eventId' = event_id
  AND envelope ->> 'eventType' = event_type))
);
CREATE INDEX IF NOT EXISTS communication_notification_publications_due
 ON communication_notification_publications(next_attempt_at) WHERE state = 'PENDING';
CREATE INDEX IF NOT EXISTS communication_notification_publications_retention
 ON communication_notification_publications(completed_at) WHERE state <> 'PENDING';
