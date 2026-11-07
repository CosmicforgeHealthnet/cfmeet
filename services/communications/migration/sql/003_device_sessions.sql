-- Blueprint v1.0 Section 14 (ACC-008, ADR 0057 amendment A1). Additive only.
-- A device reference remembers the Account session (sign-in) that linked it, as the
-- opaque domain-separated reference Account computes; the Hub can match it, never
-- reverse it. Ending that session ends the device; revoking the device ends the session.
ALTER TABLE communication_devices ADD COLUMN IF NOT EXISTS account_session_ref VARCHAR(64)
 CHECK (account_session_ref IS NULL OR account_session_ref ~ '^[0-9a-f]{64}$');
CREATE INDEX IF NOT EXISTS communication_devices_account_session
 ON communication_devices(profile_id,account_session_ref) WHERE account_session_ref IS NOT NULL AND state='ACTIVE';
-- One more terminal reason. The previous image never writes it and only echoes it.
ALTER TABLE communication_devices DROP CONSTRAINT IF EXISTS communication_devices_revocation_reason_check;
ALTER TABLE communication_devices ADD CONSTRAINT communication_devices_revocation_reason_check
 CHECK (revocation_reason IN ('USER_REQUESTED','COMPROMISED','OPERATOR','OWNER_SERVICE','ACCOUNT_SESSION_ENDED'));
CREATE OR REPLACE FUNCTION communication_device_session_binding_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.account_session_ref IS NOT NULL AND NEW.account_session_ref IS DISTINCT FROM OLD.account_session_ref THEN
  RAISE EXCEPTION 'device session binding is immutable' USING ERRCODE='integrity_constraint_violation';
 END IF;
 RETURN NEW;
END; $$;
DROP TRIGGER IF EXISTS communication_device_session_binding ON communication_devices;
CREATE TRIGGER communication_device_session_binding BEFORE UPDATE ON communication_devices
 FOR EACH ROW EXECUTE FUNCTION communication_device_session_binding_immutable();
