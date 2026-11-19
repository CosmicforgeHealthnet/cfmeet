-- Short CF Meet IDs and direct contact (owner decision, 19 November 2026).
--
-- 1. CF Meet IDs become CF- plus 6 characters from an alphabet without look-alikes
--    (no I, O, 0 or 1). Every profile whose current ID is a long one gets a new short ID;
--    the long one is retired, so it no longer resolves (owner decision: old IDs stop
--    working). Retired long rows stay for history, so the CHECK admits both shapes.
-- 2. contact_approval: AUTOMATIC (default, owner decision: on for everyone) — anyone who
--    has the person's CF Meet ID is connected straight away when they add them, and can
--    then chat and call; MANUAL keeps contact requests. An older image ignores the column.
--
-- Rollback: an older image keeps starting (additive; it ignores this history), but it only
-- accepts long IDs, so finding people by their new short ID fails until this image is back.
DO $$
DECLARE
    c TEXT;
BEGIN
    SELECT conname INTO c FROM pg_constraint
     WHERE conrelid = 'communication_public_ids'::regclass AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%CF-[A-Z2-7]{26}%';
    IF c IS NOT NULL THEN
        EXECUTE format('ALTER TABLE communication_public_ids DROP CONSTRAINT %I', c);
    END IF;
END $$;
ALTER TABLE communication_public_ids ADD CONSTRAINT ck_communication_public_ids_shape
    CHECK (public_id ~ '^CF-[A-Z2-7]{26}$' OR public_id ~ '^CF-[A-HJ-NP-Z2-9]{6}$');

DO $$
DECLARE
    r RECORD;
    code TEXT;
    tries INT;
    alphabet CONSTANT TEXT := 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
BEGIN
    FOR r IN SELECT profile_id FROM communication_public_ids
              WHERE current AND public_id ~ '^CF-[A-Z2-7]{26}$' LOOP
        UPDATE communication_public_ids SET current = false, retired_at = now()
         WHERE profile_id = r.profile_id AND current;
        tries := 0;
        LOOP
            code := 'CF-' || (SELECT string_agg(substr(alphabet, 1 + floor(random() * 32)::int, 1), '')
                                FROM generate_series(1, 6));
            BEGIN
                INSERT INTO communication_public_ids(public_id, profile_id) VALUES (code, r.profile_id);
                EXIT;
            EXCEPTION WHEN unique_violation THEN
                tries := tries + 1;
                IF tries > 100 THEN RAISE; END IF;
            END;
        END LOOP;
    END LOOP;
END $$;

ALTER TABLE communication_privacy ADD COLUMN IF NOT EXISTS contact_approval TEXT NOT NULL DEFAULT 'AUTOMATIC';
ALTER TABLE communication_privacy ADD CONSTRAINT ck_communication_privacy_contact_approval
    CHECK (contact_approval IN ('AUTOMATIC', 'MANUAL'));
