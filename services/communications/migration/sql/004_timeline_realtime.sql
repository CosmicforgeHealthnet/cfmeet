-- Audit closure MSG-009. Additive only.
-- 1. Call cards owed to call parties who had no Hub profile when the call was launched
--    from a conversation. Calling decided they are parties; the card is written for them
--    (and only them) the first time they read their timeline or activity with a profile.
--    References and safe state only; pruned after 30 days (call history still lists it).
CREATE TABLE IF NOT EXISTS communication_pending_call_cards (
 tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 principal_id UUID NOT NULL, conversation_id UUID NOT NULL,
 entry_key VARCHAR(120) NOT NULL CHECK (entry_key ~ '^call:[0-9a-f-]{36}$'),
 call_id UUID NOT NULL, occurred_at TIMESTAMPTZ NOT NULL, data JSONB NOT NULL DEFAULT '{}',
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY (tenant_id,application_id,environment_id,principal_id,conversation_id,entry_key)
);
CREATE INDEX IF NOT EXISTS communication_pending_call_cards_retention ON communication_pending_call_cards(created_at);
-- 2. Realtime nudges for conversation timelines. One row per distinct change (a call
--    started, a Calling state version observed, a meeting shared): a redelivered or
--    repeated observation of the same change is deduplicated here, so it never bumps the
--    conversation's timeline version twice.
CREATE TABLE IF NOT EXISTS communication_timeline_nudges (
 conversation_id UUID NOT NULL, nudge_key VARCHAR(160) NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY (conversation_id,nudge_key)
);
CREATE INDEX IF NOT EXISTS communication_timeline_nudges_retention ON communication_timeline_nudges(created_at);
-- 3. Monotonic per-conversation timeline version announced to Messaging.
CREATE TABLE IF NOT EXISTS communication_timeline_versions (
 conversation_id UUID PRIMARY KEY,
 tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 version BIGINT NOT NULL CHECK (version>=1), updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
