-- Explicit, Account-authorized promotion only. Existing links retain their legacy policy.
CREATE TABLE communication_messaging_installations (
    tenant_id UUID NOT NULL,
    application_id UUID NOT NULL,
    environment_id UUID NOT NULL,
    principal_id UUID NOT NULL,
    chat_device_id UUID NOT NULL,
    installation_ref CHAR(64) NOT NULL CHECK (installation_ref ~ '^[0-9a-f]{64}$'),
    original_account_session_ref VARCHAR(64) NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(tenant_id, application_id, environment_id, chat_device_id)
);
CREATE OR REPLACE FUNCTION communication_device_session_binding_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.account_session_ref IS NOT NULL AND NEW.account_session_ref IS DISTINCT FROM OLD.account_session_ref THEN
  -- A proved chat-only installation no longer belongs to its original sign-in. Clearing
  -- this legacy pointer also keeps older Hub images from destroying it after rollback.
  IF NEW.account_session_ref IS NOT NULL OR NEW.call_endpoint_ref IS NOT NULL OR
     NOT EXISTS (
        SELECT 1 FROM communication_messaging_installations i
        JOIN communication_profiles p ON p.id = NEW.profile_id
        WHERE i.tenant_id=p.tenant_id AND i.application_id=p.application_id
          AND i.environment_id=p.environment_id AND i.principal_id=p.principal_id
          AND i.chat_device_id=NEW.chat_device_ref
     ) THEN
   RAISE EXCEPTION 'device session binding is immutable' USING ERRCODE='integrity_constraint_violation';
  END IF;
 END IF;
 RETURN NEW;
END; $$;
