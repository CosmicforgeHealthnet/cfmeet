CREATE TABLE communication_profiles (
 id UUID PRIMARY KEY, tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 account_reference VARCHAR(80) NOT NULL, principal_id UUID NOT NULL,
 display_name VARCHAR(160) NOT NULL, avatar_reference VARCHAR(512),
 activation_state TEXT NOT NULL DEFAULT 'ACCOUNT_CREATED' CHECK (activation_state IN ('ACCOUNT_CREATED','ACTIVE','SUSPENDED','DEACTIVATED')),
 version BIGINT NOT NULL DEFAULT 1 CHECK(version>0), account_version BIGINT NOT NULL DEFAULT 0,
 credentials_not_before TIMESTAMPTZ, suspended_at TIMESTAMPTZ, deactivated_at TIMESTAMPTZ,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE(tenant_id,application_id,environment_id,account_reference),
 UNIQUE(tenant_id,application_id,environment_id,principal_id),
 UNIQUE(tenant_id,application_id,environment_id,id)
);
CREATE TABLE communication_scope_authorities (
 tenant_id UUID NOT NULL,application_id UUID NOT NULL,environment_id UUID NOT NULL,
 service_account_id UUID NOT NULL,environment_kind TEXT NOT NULL CHECK(environment_kind IN ('SANDBOX','TEST','PRODUCTION')),
 PRIMARY KEY(tenant_id,application_id,environment_id)
);
CREATE TABLE communication_public_ids (
 public_id VARCHAR(40) PRIMARY KEY CHECK(public_id ~ '^CF-[A-Z2-7]{26}$'),
 profile_id UUID NOT NULL REFERENCES communication_profiles(id), current BOOLEAN NOT NULL DEFAULT true,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), retired_at TIMESTAMPTZ,
 CHECK ((current AND retired_at IS NULL) OR (NOT current AND retired_at IS NOT NULL))
);
CREATE UNIQUE INDEX communication_one_current_public_id ON communication_public_ids(profile_id) WHERE current;
CREATE TABLE communication_privacy (
 profile_id UUID PRIMARY KEY REFERENCES communication_profiles(id), version BIGINT NOT NULL DEFAULT 1 CHECK(version>0),
 public_id_discoverable BOOLEAN NOT NULL DEFAULT true,
 invitation_permissions TEXT NOT NULL DEFAULT 'ENABLED' CHECK(invitation_permissions IN ('ENABLED','DISABLED')),
 contact_request_permissions TEXT NOT NULL DEFAULT 'EVERYONE' CHECK(contact_request_permissions IN ('EVERYONE','NOBODY')),
 incoming_call_policy TEXT NOT NULL DEFAULT 'CONNECTIONS' CHECK(incoming_call_policy IN ('CONNECTIONS','NOBODY')),
 direct_chat_policy TEXT NOT NULL DEFAULT 'CONNECTIONS' CHECK(direct_chat_policy IN ('CONNECTIONS','NOBODY')),
 public_profile_visibility TEXT NOT NULL DEFAULT 'PUBLIC' CHECK(public_profile_visibility IN ('PUBLIC','CONNECTIONS','HIDDEN')),
 presence_visibility TEXT NOT NULL DEFAULT 'CONNECTIONS' CHECK(presence_visibility IN ('CONNECTIONS','NOBODY')),
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE communication_invites (
 id UUID PRIMARY KEY, owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 token_digest BYTEA NOT NULL UNIQUE CHECK(octet_length(token_digest)=32), purpose TEXT NOT NULL CHECK(purpose='CONNECT'),
 expires_at TIMESTAMPTZ NOT NULL, revoked_at TIMESTAMPTZ, max_uses INTEGER NOT NULL CHECK(max_uses BETWEEN 1 AND 10),
 uses INTEGER NOT NULL DEFAULT 0 CHECK(uses>=0 AND uses<=max_uses), version BIGINT NOT NULL DEFAULT 1 CHECK(version>0),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), CHECK(expires_at>created_at)
);
CREATE INDEX communication_invites_owner_page ON communication_invites(owner_profile_id,created_at DESC,id);
CREATE INDEX communication_invites_expiry ON communication_invites(expires_at) WHERE revoked_at IS NULL;
CREATE TABLE communication_invite_claims (
 invite_id UUID NOT NULL REFERENCES communication_invites(id), requester_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 claimed_at TIMESTAMPTZ NOT NULL DEFAULT now(), idempotency_key UUID NOT NULL, PRIMARY KEY(invite_id,requester_profile_id)
);
CREATE TABLE communication_connections (
 id UUID PRIMARY KEY, tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 left_profile_id UUID NOT NULL, right_profile_id UUID NOT NULL,
 state TEXT NOT NULL DEFAULT 'NONE' CHECK(state IN ('NONE','REQUEST_PENDING','CONNECTED','BLOCKED','REVOKED')),
 version BIGINT NOT NULL DEFAULT 1 CHECK(version>0), connected_at TIMESTAMPTZ, revoked_at TIMESTAMPTZ,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK(left_profile_id<right_profile_id), UNIQUE(tenant_id,application_id,environment_id,left_profile_id,right_profile_id),
 FOREIGN KEY(tenant_id,application_id,environment_id,left_profile_id) REFERENCES communication_profiles(tenant_id,application_id,environment_id,id),
 FOREIGN KEY(tenant_id,application_id,environment_id,right_profile_id) REFERENCES communication_profiles(tenant_id,application_id,environment_id,id)
);
CREATE INDEX communication_connections_right ON communication_connections(right_profile_id,state,updated_at DESC,id);
CREATE INDEX communication_connections_left ON communication_connections(left_profile_id,state,updated_at DESC,id);
CREATE TABLE communication_contact_requests (
 id UUID PRIMARY KEY, connection_id UUID NOT NULL REFERENCES communication_connections(id),
 sender_profile_id UUID NOT NULL REFERENCES communication_profiles(id), recipient_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 state TEXT NOT NULL DEFAULT 'PENDING' CHECK(state IN ('PENDING','ACCEPTED','DECLINED','CANCELLED','EXPIRED')),
 version BIGINT NOT NULL DEFAULT 1 CHECK(version>0), created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '30 days',
 CHECK(sender_profile_id<>recipient_profile_id)
);
CREATE UNIQUE INDEX communication_one_pending_request ON communication_contact_requests(connection_id) WHERE state='PENDING';
CREATE INDEX communication_requests_incoming ON communication_contact_requests(recipient_profile_id,state,created_at DESC,id);
CREATE INDEX communication_requests_outgoing ON communication_contact_requests(sender_profile_id,state,created_at DESC,id);
CREATE TABLE communication_contact_aliases (
 id UUID PRIMARY KEY, owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 connection_id UUID NOT NULL REFERENCES communication_connections(id), alias VARCHAR(160), favorite BOOLEAN NOT NULL DEFAULT false,
 notifications TEXT NOT NULL DEFAULT 'ALL' CHECK(notifications IN ('ALL','MUTED','MENTIONS')),
 version BIGINT NOT NULL DEFAULT 1 CHECK(version>0), created_at TIMESTAMPTZ NOT NULL DEFAULT now(),updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE(owner_profile_id,connection_id)
);
CREATE INDEX communication_contacts_page ON communication_contact_aliases(owner_profile_id,created_at DESC,id);
CREATE TABLE communication_block_bridge (
 blocker_profile_id UUID NOT NULL REFERENCES communication_profiles(id), blocked_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 source TEXT NOT NULL CHECK(source IN ('COMMUNICATIONS','CALLING','MESSAGING')), active BOOLEAN NOT NULL,
 source_version BIGINT NOT NULL DEFAULT 1 CHECK(source_version>0), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(blocker_profile_id,blocked_profile_id,source), CHECK(blocker_profile_id<>blocked_profile_id)
);
CREATE INDEX communication_blocks_reverse ON communication_block_bridge(blocked_profile_id,blocker_profile_id) WHERE active;
CREATE TABLE communication_idempotency (
 owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id), operation VARCHAR(100) NOT NULL,
 key UUID NOT NULL, request_digest BYTEA NOT NULL CHECK(octet_length(request_digest)=32), response JSONB,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '24 hours',
 PRIMARY KEY(owner_profile_id,operation,key)
);
CREATE INDEX communication_idempotency_expiry ON communication_idempotency(expires_at);
CREATE TABLE communication_rate_windows (
 owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id), operation VARCHAR(40) NOT NULL,
 window_start TIMESTAMPTZ NOT NULL, count INTEGER NOT NULL CHECK(count>0), PRIMARY KEY(owner_profile_id,operation,window_start)
);
CREATE TABLE communication_inbox_dedupe (
 source VARCHAR(60) NOT NULL, event_id UUID NOT NULL, request_digest BYTEA NOT NULL CHECK(octet_length(request_digest)=32),
 received_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY(source,event_id)
);
CREATE TABLE communication_service_nonces (
 issuer VARCHAR(64) NOT NULL, token_id VARCHAR(80) NOT NULL, expires_at TIMESTAMPTZ NOT NULL,
 PRIMARY KEY(issuer,token_id)
);
CREATE INDEX communication_service_nonces_expiry ON communication_service_nonces(expires_at);
CREATE TABLE communication_outbox (
 seq BIGSERIAL PRIMARY KEY, event_id UUID NOT NULL, tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 destination TEXT NOT NULL CHECK(destination IN ('CALLING','MESSAGING')), aggregate_id UUID NOT NULL, aggregate_version BIGINT NOT NULL,
 event_type VARCHAR(80) NOT NULL, payload JSONB NOT NULL, state TEXT NOT NULL DEFAULT 'PENDING' CHECK(state IN ('PENDING','INFLIGHT','DELIVERED','DEAD')),
 attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0), next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 lease_id UUID, lease_until TIMESTAMPTZ, delivered_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE(event_id,destination)
);
CREATE INDEX communication_outbox_due ON communication_outbox(next_attempt_at,seq) WHERE state IN ('PENDING','INFLIGHT');
CREATE TABLE communication_audit (
 seq BIGSERIAL PRIMARY KEY, tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 actor_profile_id UUID, operation VARCHAR(80) NOT NULL, resource_id UUID, details JSONB NOT NULL DEFAULT '{}',
 created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX communication_audit_retention ON communication_audit(created_at);
CREATE TABLE communication_profile_reports (
 id UUID PRIMARY KEY, reporter_profile_id UUID NOT NULL REFERENCES communication_profiles(id), target_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 reason TEXT NOT NULL CHECK(reason IN ('SPAM','HARASSMENT','IMPERSONATION','OTHER')),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), CHECK(reporter_profile_id<>target_profile_id)
);
CREATE FUNCTION communication_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'communications audit is append-only' USING ERRCODE='integrity_constraint_violation'; END; $$;
CREATE TRIGGER communication_audit_immutable BEFORE UPDATE OR DELETE ON communication_audit FOR EACH ROW EXECUTE FUNCTION communication_append_only();
CREATE FUNCTION communication_guard_version() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF NEW.version<>OLD.version+1 THEN RAISE EXCEPTION 'communications version must advance by one' USING ERRCODE='integrity_constraint_violation'; END IF; RETURN NEW; END; $$;
CREATE TRIGGER communication_profile_version BEFORE UPDATE ON communication_profiles FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE TRIGGER communication_privacy_version BEFORE UPDATE ON communication_privacy FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE TRIGGER communication_connection_version BEFORE UPDATE ON communication_connections FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE TRIGGER communication_request_version BEFORE UPDATE ON communication_contact_requests FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE TRIGGER communication_alias_version BEFORE UPDATE ON communication_contact_aliases FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE TRIGGER communication_invite_version BEFORE UPDATE ON communication_invites FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
-- Authority bindings are immutable. Rotation changes only the public identifier.
CREATE FUNCTION communication_profile_identity_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF ROW(NEW.id,NEW.tenant_id,NEW.application_id,NEW.environment_id,NEW.account_reference,NEW.principal_id)
 IS DISTINCT FROM ROW(OLD.id,OLD.tenant_id,OLD.application_id,OLD.environment_id,OLD.account_reference,OLD.principal_id)
 THEN RAISE EXCEPTION 'communications identity is immutable' USING ERRCODE='integrity_constraint_violation'; END IF;
 IF NEW.account_version<OLD.account_version OR (OLD.credentials_not_before IS NOT NULL AND (NEW.credentials_not_before IS NULL OR NEW.credentials_not_before<OLD.credentials_not_before))
 THEN RAISE EXCEPTION 'communications authority cannot move backwards' USING ERRCODE='integrity_constraint_violation'; END IF;
 IF OLD.activation_state='DEACTIVATED' AND NEW.activation_state<>'DEACTIVATED'
 THEN RAISE EXCEPTION 'deactivation is terminal' USING ERRCODE='integrity_constraint_violation'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER communication_profile_identity BEFORE UPDATE ON communication_profiles FOR EACH ROW EXECUTE FUNCTION communication_profile_identity_guard();
CREATE FUNCTION communication_pair_reference_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a UUID; b UUID;
BEGIN
 SELECT left_profile_id,right_profile_id INTO a,b FROM communication_connections WHERE id=NEW.connection_id;
 IF TG_TABLE_NAME='communication_contact_requests' THEN
  IF LEAST(NEW.sender_profile_id,NEW.recipient_profile_id) IS DISTINCT FROM a OR GREATEST(NEW.sender_profile_id,NEW.recipient_profile_id) IS DISTINCT FROM b
  THEN RAISE EXCEPTION 'request does not belong to relationship' USING ERRCODE='integrity_constraint_violation'; END IF;
 ELSE
  IF NEW.owner_profile_id IS DISTINCT FROM a AND NEW.owner_profile_id IS DISTINCT FROM b
  THEN RAISE EXCEPTION 'contact does not belong to owner' USING ERRCODE='integrity_constraint_violation'; END IF;
 END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER communication_request_pair BEFORE INSERT OR UPDATE ON communication_contact_requests FOR EACH ROW EXECUTE FUNCTION communication_pair_reference_guard();
CREATE TRIGGER communication_contact_owner BEFORE INSERT OR UPDATE ON communication_contact_aliases FOR EACH ROW EXECUTE FUNCTION communication_pair_reference_guard();
CREATE FUNCTION communication_block_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM communication_profiles a JOIN communication_profiles b
 ON a.tenant_id=b.tenant_id AND a.application_id=b.application_id AND a.environment_id=b.environment_id
 WHERE a.id=NEW.blocker_profile_id AND b.id=NEW.blocked_profile_id)
 THEN RAISE EXCEPTION 'block scope mismatch' USING ERRCODE='integrity_constraint_violation'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER communication_block_scope BEFORE INSERT OR UPDATE ON communication_block_bridge FOR EACH ROW EXECUTE FUNCTION communication_block_scope_guard();
