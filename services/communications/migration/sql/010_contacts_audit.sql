-- Contacts audit (owner request, 19 November 2026): schema for the contact fixes. Every
-- change is additive, so the previous image keeps starting and working after a rollback
-- (it checks its own histories only and never names these columns or tables).
--
-- 1. K5 unblocking: remember that a pair was CONNECTED when it was blocked, so unblocking
--    restores the contact (WhatsApp keeps the contact) unless either person removed the
--    contact meanwhile. An older image ignores the column and unblocks to REVOKED.
ALTER TABLE communication_connections
    ADD COLUMN IF NOT EXISTS connected_before_block BOOLEAN NOT NULL DEFAULT false;

-- 2. KF1 find people by verified email: off unless the person turns it on (opt-in; an
--    email address is personal data and a match would confirm the person uses CF Meet).
ALTER TABLE communication_privacy
    ADD COLUMN IF NOT EXISTS email_discoverable BOOLEAN NOT NULL DEFAULT false;

-- 3. KF2 "About" line contacts can see (at most 140 characters, no control characters;
--    the application validates, the CHECK is the backstop).
ALTER TABLE communication_profiles ADD COLUMN IF NOT EXISTS about VARCHAR(140);
ALTER TABLE communication_profiles ADD CONSTRAINT ck_communication_profiles_about
    CHECK (about IS NULL OR (char_length(about) <= 140 AND about !~ '[[:cntrl:]]'));

-- 4. K8 change feed positions. `seq` is one shared BIGSERIAL numbered when a row is written
--    but visible only when its transaction commits, so with overlapping transactions a
--    device could read past a smaller number that committed later and never see it.
--    Each person's entries now also get a `position` from a per-person counter, assigned
--    under that person's counter row lock when the transaction commits (a deferred
--    constraint trigger), so a person's positions become visible in increasing order.
--    The lock is held only for the commit, never for the whole transaction.
--
--    position = GREATEST(counter + 1, seq): it never falls below seq, so
--      * a cursor a device got from an older image (seq numbers) still finds every later
--        entry (they all have position >= seq > cursor), and
--      * an older image after a rollback keeps reading seq; the trigger keeps numbering
--        its inserts too, so rolling forward again is safe.
--    Rows written before this migration keep position NULL: readers use
--    COALESCE(position, seq), which equals seq for them (the numbers devices already hold).
ALTER TABLE communication_changes ADD COLUMN IF NOT EXISTS position BIGINT;
CREATE TABLE IF NOT EXISTS communication_change_heads (
    profile_id UUID PRIMARY KEY REFERENCES communication_profiles(id) ON DELETE CASCADE,
    position BIGINT NOT NULL CHECK (position > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS communication_changes_position
    ON communication_changes (profile_id, (COALESCE(position, seq)));
CREATE OR REPLACE FUNCTION communication_change_position() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    assigned BIGINT;
BEGIN
    INSERT INTO communication_change_heads AS h (profile_id, position) VALUES (NEW.profile_id, NEW.seq)
    ON CONFLICT (profile_id) DO UPDATE SET position = GREATEST(h.position + 1, EXCLUDED.position), updated_at = now()
    RETURNING h.position INTO assigned;
    UPDATE communication_changes SET position = assigned WHERE seq = NEW.seq;
    RETURN NULL;
END; $$;
DROP TRIGGER IF EXISTS communication_change_position ON communication_changes;
CREATE CONSTRAINT TRIGGER communication_change_position AFTER INSERT ON communication_changes
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION communication_change_position();
