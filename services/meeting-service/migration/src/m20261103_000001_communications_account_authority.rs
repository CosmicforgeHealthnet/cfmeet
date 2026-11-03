use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection().execute_unprepared(r#"
ALTER TYPE user_status ADD VALUE IF NOT EXISTS 'suspended';
CREATE TABLE account_communication_state (
 user_id UUID PRIMARY KEY REFERENCES users(id), version BIGINT NOT NULL DEFAULT 1 CHECK(version>0), credentials_not_before TIMESTAMPTZ
);
CREATE TABLE account_communication_outbox (
 event_id UUID PRIMARY KEY,payload JSONB NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 attempts INTEGER NOT NULL DEFAULT 0,next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 lease_id UUID,lease_until TIMESTAMPTZ,delivered_at TIMESTAMPTZ
);
CREATE INDEX account_communication_outbox_due ON account_communication_outbox(next_attempt_at,event_id) WHERE delivered_at IS NULL;
CREATE FUNCTION account_communication_status_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE revision BIGINT; ev UUID; state_name TEXT;
BEGIN
 IF NEW.status IS NOT DISTINCT FROM OLD.status AND NEW.password_hash IS NOT DISTINCT FROM OLD.password_hash THEN RETURN NEW; END IF;
 INSERT INTO account_communication_state(user_id,version,credentials_not_before) VALUES(NEW.id,1,CASE WHEN (lower(NEW.status::text) IN ('inactive','suspended') OR NEW.password_hash IS DISTINCT FROM OLD.password_hash) THEN now() END)
 ON CONFLICT(user_id) DO UPDATE SET version=account_communication_state.version+1,
 credentials_not_before=CASE WHEN (lower(NEW.status::text) IN ('inactive','suspended') OR NEW.password_hash IS DISTINCT FROM OLD.password_hash) THEN now() ELSE account_communication_state.credentials_not_before END RETURNING version INTO revision;
 state_name:=CASE lower(NEW.status::text) WHEN 'inactive' THEN 'DEACTIVATED' WHEN 'suspended' THEN 'SUSPENDED' ELSE 'ACCOUNT_CREATED' END;
 ev:=gen_random_uuid();
 INSERT INTO account_communication_outbox(event_id,payload) VALUES(ev,jsonb_build_object(
  'eventId',ev,'accountReference','cfmeet-user:'||NEW.id::text,'sourceVersion',revision,
  'kind',CASE WHEN NEW.password_hash IS DISTINCT FROM OLD.password_hash THEN 'SESSION_REVOKED' ELSE 'STATUS_CHANGED' END,'state',state_name,'occurredAt',now()));
 RETURN NEW;
END; $$;
CREATE TRIGGER account_communication_status_outbox AFTER UPDATE OF status,password_hash ON users FOR EACH ROW EXECUTE FUNCTION account_communication_status_changed();
"#).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Account communications authority migration is additive and forward-only".into(),
        ))
    }
}
