-- Notification Service roadmap N10 (Document 5 §6, Phase 4.2): the explicit switch of the
-- Hub notification preference endpoints. Additive only.
--   HUB     (default, today's behaviour): the Hub reads and writes its own preference rows.
--   FREEZE  the short write freeze around the final copy pass: preference reads stay local,
--           preference writes answer 503 + Retry-After (clients retry).
--   SERVICE the endpoints are facades over the Notification Service (U6 / U7), the single
--           preference writer; the Hub keeps its rows as a mirror of what it relayed, so the
--           Hub readers that have not moved (policy/check for Messaging push, the Meeting
--           opt-out read, sync overlays) and a rollback to HUB keep every change made
--           through these endpoints.
-- One row, read on every preference request; changed only by the release tooling
-- (version-guarded, journaled). Rollback: set mode back to HUB.
CREATE TABLE IF NOT EXISTS communication_preference_authority (
 singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
 mode TEXT NOT NULL DEFAULT 'HUB' CHECK (mode IN ('HUB','FREEZE','SERVICE')),
 version BIGINT NOT NULL DEFAULT 1 CHECK (version>0),
 changed_by_run VARCHAR(64) NOT NULL DEFAULT 'migration',
 changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO communication_preference_authority(singleton) VALUES (true) ON CONFLICT DO NOTHING;
DROP TRIGGER IF EXISTS communication_preference_authority_version ON communication_preference_authority;
CREATE TRIGGER communication_preference_authority_version BEFORE UPDATE ON communication_preference_authority FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
