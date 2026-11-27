-- Gap audit A16: an account reactivated by its owner or an administrator is no longer
-- stuck DEACTIVATED in the Hub. The only way back is DEACTIVATED -> ACCOUNT_CREATED (the
-- person activates the Hub again); credentials_not_before still only moves forward, so
-- every credential issued before the deactivation stays invalid.
CREATE OR REPLACE FUNCTION communication_profile_identity_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.id,NEW.tenant_id,NEW.application_id,NEW.environment_id,NEW.account_reference,NEW.principal_id)
 IS DISTINCT FROM ROW(OLD.id,OLD.tenant_id,OLD.application_id,OLD.environment_id,OLD.account_reference,OLD.principal_id)
 THEN RAISE EXCEPTION 'communications identity is immutable' USING ERRCODE='integrity_constraint_violation'; END IF;
 IF NEW.account_version<OLD.account_version OR (OLD.credentials_not_before IS NOT NULL AND (NEW.credentials_not_before IS NULL OR NEW.credentials_not_before<OLD.credentials_not_before))
 THEN RAISE EXCEPTION 'communications authority cannot move backwards' USING ERRCODE='integrity_constraint_violation'; END IF;
 IF OLD.activation_state='DEACTIVATED' AND NEW.activation_state NOT IN ('DEACTIVATED','ACCOUNT_CREATED')
 THEN RAISE EXCEPTION 'a deactivated profile only returns through reactivation' USING ERRCODE='integrity_constraint_violation'; END IF;
 RETURN NEW;
END; $$;
