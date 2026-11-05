-- Sections 9–23: unified Communications projections and per-user controls.
-- Additive only. Canonical calls, messages, conversations, meetings and devices stay
-- with their owners; these tables hold per-user preferences and opaque references.
-- No message content, ciphertext, keys, push tokens or credentials are stored.

-- Optional stable ordering for existing per-owner favorites (NULL = unordered).
ALTER TABLE communication_contact_aliases ADD COLUMN favorite_rank INTEGER CHECK (favorite_rank BETWEEN 0 AND 9999);
CREATE INDEX communication_favorites_page ON communication_contact_aliases(owner_profile_id,favorite_rank,id) WHERE favorite;

-- Private contact lists. A list never grants communication permission and deleting it
-- never deletes contacts (members cascade; aliases are untouched).
CREATE TABLE communication_contact_lists (
 id UUID PRIMARY KEY, owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 name VARCHAR(80) NOT NULL CHECK (length(btrim(name))>0), position INTEGER NOT NULL DEFAULT 0 CHECK (position BETWEEN 0 AND 9999),
 version BIGINT NOT NULL DEFAULT 1 CHECK (version>0),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE (owner_profile_id,id)
);
CREATE UNIQUE INDEX communication_contact_list_name ON communication_contact_lists(owner_profile_id,lower(name));
CREATE INDEX communication_contact_lists_page ON communication_contact_lists(owner_profile_id,position,id);
CREATE TRIGGER communication_contact_list_version BEFORE UPDATE ON communication_contact_lists FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE TABLE communication_contact_list_members (
 list_id UUID NOT NULL, owner_profile_id UUID NOT NULL, contact_id UUID NOT NULL REFERENCES communication_contact_aliases(id) ON DELETE CASCADE,
 added_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY (list_id,contact_id),
 FOREIGN KEY (owner_profile_id,list_id) REFERENCES communication_contact_lists(owner_profile_id,id) ON DELETE CASCADE
);
CREATE INDEX communication_contact_list_members_contact ON communication_contact_list_members(contact_id);
CREATE FUNCTION communication_list_member_owner_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM communication_contact_aliases a WHERE a.id=NEW.contact_id AND a.owner_profile_id=NEW.owner_profile_id)
 THEN RAISE EXCEPTION 'list member must be the owner''s own contact' USING ERRCODE='integrity_constraint_violation'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER communication_list_member_owner BEFORE INSERT OR UPDATE ON communication_contact_list_members FOR EACH ROW EXECUTE FUNCTION communication_list_member_owner_guard();

-- Per-user, per-conversation preferences. Conversation ids are Messaging references
-- (no cross-database foreign key). Disappearing-message policy stays with Messaging.
CREATE TABLE communication_conversation_preferences (
 owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id), conversation_id UUID NOT NULL,
 notifications TEXT NOT NULL DEFAULT 'ALL' CHECK (notifications IN ('ALL','MENTIONS','MUTED')),
 muted_until TIMESTAMPTZ,
 media_auto_download TEXT NOT NULL DEFAULT 'WIFI' CHECK (media_auto_download IN ('ALWAYS','WIFI','NEVER')),
 version BIGINT NOT NULL DEFAULT 1 CHECK (version>0),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY (owner_profile_id,conversation_id),
 CHECK (muted_until IS NULL OR notifications='MUTED')
);
CREATE TRIGGER communication_conversation_preference_version BEFORE UPDATE ON communication_conversation_preferences FOR EACH ROW EXECUTE FUNCTION communication_guard_version();

-- "Clear chat for me": a monotonic per-user visibility watermark. Canonical Messaging
-- rows, other participants' history, retention and legal holds are unaffected.
CREATE TABLE communication_conversation_watermarks (
 owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id), conversation_id UUID NOT NULL,
 cleared_through_sequence BIGINT NOT NULL CHECK (cleared_through_sequence>=0), cleared_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 version BIGINT NOT NULL DEFAULT 1 CHECK (version>0), PRIMARY KEY (owner_profile_id,conversation_id)
);
CREATE FUNCTION communication_watermark_monotonic() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.cleared_through_sequence<OLD.cleared_through_sequence OR NEW.version<>OLD.version+1
 THEN RAISE EXCEPTION 'clear-for-me watermark cannot move backwards' USING ERRCODE='integrity_constraint_violation'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER communication_watermark_guard BEFORE UPDATE ON communication_conversation_watermarks FOR EACH ROW EXECUTE FUNCTION communication_watermark_monotonic();

-- Starred/saved message references only (never content).
CREATE TABLE communication_saved_messages (
 id UUID PRIMARY KEY, owner_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 conversation_id UUID NOT NULL, message_id UUID NOT NULL, message_sequence BIGINT NOT NULL CHECK (message_sequence>0),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), UNIQUE (owner_profile_id,message_id)
);
CREATE INDEX communication_saved_messages_page ON communication_saved_messages(owner_profile_id,created_at DESC,id DESC);
CREATE INDEX communication_saved_messages_conversation ON communication_saved_messages(owner_profile_id,conversation_id,message_sequence);

-- Account-wide notification preferences. Security/session events are always delivered.
CREATE TABLE communication_notification_preferences (
 profile_id UUID PRIMARY KEY REFERENCES communication_profiles(id), version BIGINT NOT NULL DEFAULT 1 CHECK (version>0),
 direct_messages BOOLEAN NOT NULL DEFAULT true, group_messages BOOLEAN NOT NULL DEFAULT true,
 incoming_calls BOOLEAN NOT NULL DEFAULT true, missed_calls BOOLEAN NOT NULL DEFAULT true,
 contact_requests BOOLEAN NOT NULL DEFAULT true, group_invitations BOOLEAN NOT NULL DEFAULT true,
 meeting_invitations BOOLEAN NOT NULL DEFAULT true,
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER communication_notification_preference_version BEFORE UPDATE ON communication_notification_preferences FOR EACH ROW EXECUTE FUNCTION communication_guard_version();

-- Unified device registry: references to the canonical Messaging device and Calling
-- endpoint, each verified with its owner for the same principal before linking.
CREATE TABLE communication_devices (
 id UUID PRIMARY KEY, profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 platform TEXT NOT NULL CHECK (platform IN ('IOS','ANDROID','WEB','DESKTOP')), label VARCHAR(80),
 chat_device_ref UUID, call_endpoint_ref UUID,
 push_platform TEXT CHECK (push_platform IN ('APNS','FCM','WEB_PUSH','LOCAL_TEST')), push_registered_at TIMESTAMPTZ,
 state TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (state IN ('ACTIVE','REVOKED')), revoked_at TIMESTAMPTZ,
 revocation_reason TEXT CHECK (revocation_reason IN ('USER_REQUESTED','COMPROMISED','OPERATOR','OWNER_SERVICE')),
 acknowledged_sync_position BIGINT NOT NULL DEFAULT 0 CHECK (acknowledged_sync_position>=0),
 version BIGINT NOT NULL DEFAULT 1 CHECK (version>0),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 CHECK (chat_device_ref IS NOT NULL OR call_endpoint_ref IS NOT NULL),
 CHECK ((state='ACTIVE' AND revoked_at IS NULL) OR (state='REVOKED' AND revoked_at IS NOT NULL))
);
CREATE UNIQUE INDEX communication_devices_chat_ref ON communication_devices(chat_device_ref) WHERE chat_device_ref IS NOT NULL;
CREATE UNIQUE INDEX communication_devices_call_ref ON communication_devices(call_endpoint_ref) WHERE call_endpoint_ref IS NOT NULL;
CREATE INDEX communication_devices_owner ON communication_devices(profile_id,created_at,id);
CREATE TRIGGER communication_device_version BEFORE UPDATE ON communication_devices FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
CREATE FUNCTION communication_device_revocation_terminal() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.state='REVOKED' AND NEW.state<>'REVOKED' THEN RAISE EXCEPTION 'device revocation is terminal' USING ERRCODE='integrity_constraint_violation'; END IF;
 IF NEW.profile_id<>OLD.profile_id OR NEW.chat_device_ref IS DISTINCT FROM OLD.chat_device_ref AND OLD.chat_device_ref IS NOT NULL
  OR NEW.call_endpoint_ref IS DISTINCT FROM OLD.call_endpoint_ref AND OLD.call_endpoint_ref IS NOT NULL
 THEN RAISE EXCEPTION 'device bindings are immutable' USING ERRCODE='integrity_constraint_violation'; END IF;
 IF NEW.acknowledged_sync_position<OLD.acknowledged_sync_position THEN RAISE EXCEPTION 'sync position cannot move backwards' USING ERRCODE='integrity_constraint_violation'; END IF;
 RETURN NEW;
END; $$;
CREATE TRIGGER communication_device_terminal BEFORE UPDATE ON communication_devices FOR EACH ROW EXECUTE FUNCTION communication_device_revocation_terminal();

-- Hub-launched call associations. Calling owns the call; this is a safe pointer.
CREATE TABLE communication_call_links (
 id UUID PRIMARY KEY, tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 initiator_profile_id UUID NOT NULL REFERENCES communication_profiles(id), call_id UUID NOT NULL,
 origin TEXT NOT NULL CHECK (origin IN ('CONTACT','CONVERSATION')), conversation_id UUID, contact_id UUID,
 kind TEXT NOT NULL CHECK (kind IN ('VOICE','VIDEO')), client_request_id UUID NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE (initiator_profile_id,client_request_id), UNIQUE (call_id),
 CHECK ((origin='CONVERSATION' AND conversation_id IS NOT NULL) OR (origin='CONTACT' AND contact_id IS NOT NULL))
);
CREATE INDEX communication_call_links_conversation ON communication_call_links(conversation_id,created_at) WHERE conversation_id IS NOT NULL;

-- Per-viewer timeline projection. Entries are references with safe state; ordering is
-- (occurred_at, entry_key). source_version makes out-of-order updates idempotent.
CREATE TABLE communication_timeline_entries (
 viewer_profile_id UUID NOT NULL REFERENCES communication_profiles(id), conversation_id UUID NOT NULL,
 entry_key VARCHAR(120) NOT NULL CHECK (entry_key ~ '^(call|meeting):[0-9a-f-]{36}$'),
 kind TEXT NOT NULL CHECK (kind IN ('CALL','MEETING_INVITATION')),
 source TEXT NOT NULL CHECK (source IN ('CALLING','MEETINGS')), source_id UUID NOT NULL,
 occurred_at TIMESTAMPTZ NOT NULL, source_version BIGINT NOT NULL DEFAULT 0 CHECK (source_version>=0),
 data JSONB NOT NULL DEFAULT '{}', projection_version INTEGER NOT NULL DEFAULT 1 CHECK (projection_version>0),
 refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY (viewer_profile_id,conversation_id,entry_key)
);
CREATE INDEX communication_timeline_page ON communication_timeline_entries(viewer_profile_id,conversation_id,occurred_at,entry_key);
CREATE INDEX communication_timeline_source ON communication_timeline_entries(source,source_id);
CREATE INDEX communication_timeline_activity ON communication_timeline_entries(viewer_profile_id,occurred_at DESC,entry_key);

-- Meetings linked into a conversation. Meetings remains the meeting authority.
CREATE TABLE communication_meeting_links (
 id UUID PRIMARY KEY, conversation_id UUID NOT NULL, linked_by_profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 meeting_space_id UUID NOT NULL, meeting_code VARCHAR(64) NOT NULL CHECK (meeting_code ~ '^[A-Za-z0-9-]{3,64}$'),
 origin TEXT NOT NULL CHECK (origin IN ('INSTANT','SHARED')), client_request_id UUID NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE (linked_by_profile_id,client_request_id), UNIQUE (conversation_id,meeting_space_id)
);
CREATE INDEX communication_meeting_links_page ON communication_meeting_links(conversation_id,created_at DESC,id);

-- Monotonic per-user change feed for multi-device synchronization. Entries name what
-- changed; clients refetch the authoritative resource. Pruned after 30 days; older
-- cursors receive RESYNC_REQUIRED with a snapshot instead of a silent gap.
CREATE TABLE communication_changes (
 seq BIGSERIAL PRIMARY KEY, profile_id UUID NOT NULL REFERENCES communication_profiles(id),
 domain TEXT NOT NULL CHECK (domain IN ('CONTACT','CONTACT_LIST','CONVERSATION_PREFERENCE','CLEAR_FOR_ME','SAVED_MESSAGE',
  'NOTIFICATION_PREFERENCE','DEVICE','TIMELINE','MEETING_LINK','PRIVACY','BLOCK','PROFILE')),
 resource_id UUID, resource_version BIGINT, created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX communication_changes_feed ON communication_changes(profile_id,seq);
CREATE INDEX communication_changes_retention ON communication_changes(created_at);

-- Operator suspension (audited); separate from Account-owned lifecycle state.
CREATE TABLE communication_operator_holds (
 profile_id UUID PRIMARY KEY REFERENCES communication_profiles(id), reason TEXT NOT NULL CHECK (reason IN ('ABUSE','SECURITY','LEGAL','OTHER')),
 active BOOLEAN NOT NULL, version BIGINT NOT NULL DEFAULT 1 CHECK (version>0), updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER communication_operator_hold_version BEFORE UPDATE ON communication_operator_holds FOR EACH ROW EXECUTE FUNCTION communication_guard_version();
