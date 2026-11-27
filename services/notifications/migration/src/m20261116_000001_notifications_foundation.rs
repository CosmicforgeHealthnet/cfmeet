//! Notification Service foundation (roadmap N2; Document 4 §6): the 17 service tables.
//!
//! | # | Table | Purpose |
//! |---|---|---|
//! | 1 | `notif_event_inbox` | I1 ingest dedupe `(producer, event_id)` + digest, route decision, lane, HELD / SHADOWED states |
//! | 2 | `notif_notifications` | one logical notification per recipient (`UNIQUE (recipient_ref, dedupe_key)`) |
//! | 3 | `notif_deliveries` | one per (notification, channel, target); leases; routing version |
//! | 4 | `notif_delivery_attempts` | append-only attempts |
//! | 5 | `notif_inbox_items` | the user-visible inbox (U1–U5) |
//! | 6 | `notif_schedules` | reminders, quiet-hour deferrals, deliver-after |
//! | 7 | `notif_routing` | service-side mode per (category, channel): OFF / SHADOW / ACTIVE |
//! | 8 | `notif_service_nonces` | durable `jti` replay guard |
//! | 9 | `notif_shadow_observations` | temporary shadow comparison (dropped at retirement) |
//! | 10 | `notif_email_messages` | the email ledger (port of Meeting `email_deliveries`) |
//! | 11 | `notif_email_provider_events` | provider webhook events (port of `email_delivery_events`) |
//! | 12 | `notif_email_suppressions` | bounce / complaint suppression (keyed digests) |
//! | 13 | `notif_push_endpoints` | push registrations (sealed handles) |
//! | 14 | `notif_preferences` | account categories, channel matrix, quiet hours |
//! | 15 | `notif_conversation_preferences` | conversation notification level |
//! | 16 | `notif_contact_preferences` | contact notification level |
//! | 17 | `notif_preference_change_log` | 7-day write-back log for the preference rollback |
//!
//! Deviations from the illustrative DDL (recorded in Document 10):
//! * sealed columns are `TEXT` holding `cf-webhooks::secret_box` seals (`v2.<kid>.<…>`),
//!   which already carry their key id and AEAD tag;
//! * `notif_event_inbox.shadow` marks producer shadow copies (never sent whatever the
//!   service mode), and state `HELD` is a first-class state;
//! * `notif_email_messages` keeps the rendered body as one sealed JSON document and records
//!   the configured sender profile (organization-invitation sender policy is configuration);
//! * every routing row starts `OFF` (dark deployment): nothing can be delivered until an
//!   activation operation changes a row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE FUNCTION notif_append_only() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '%: append-only', TG_TABLE_NAME USING ERRCODE = 'check_violation';
END $$;

-- 1. ingest dedupe
CREATE TABLE notif_event_inbox (
    producer          VARCHAR(64)  NOT NULL,
    event_id          VARCHAR(64)  NOT NULL,
    request_digest    CHAR(64)     NOT NULL,
    event_type        VARCHAR(96)  NOT NULL,
    event_version     SMALLINT     NOT NULL,
    route_category    VARCHAR(40)  NOT NULL,
    lane              VARCHAR(10)  NOT NULL,
    state             VARCHAR(16)  NOT NULL,
    shadow            BOOLEAN      NOT NULL DEFAULT false,
    reject_reason     VARCHAR(64),
    envelope          JSONB        NOT NULL,
    route_decision_id UUID         NOT NULL,
    routing_version   INT          NOT NULL,
    routing_protocol  SMALLINT     NOT NULL,
    received_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    processed_at      TIMESTAMPTZ,
    lease_id          UUID,
    lease_until       TIMESTAMPTZ,
    attempts          INT          NOT NULL DEFAULT 0,
    last_error_class  VARCHAR(64),
    CONSTRAINT pk_notif_event_inbox PRIMARY KEY (producer, event_id),
    CONSTRAINT ck_notif_event_inbox_producer CHECK (producer ~ '^cfmeet\.[a-z][a-z0-9-]*$|^alertmanager$'),
    CONSTRAINT ck_notif_event_inbox_lane CHECK (lane IN ('CRITICAL', 'HIGH', 'STANDARD')),
    CONSTRAINT ck_notif_event_inbox_state CHECK (state IN ('PENDING', 'PROCESSED', 'REJECTED', 'SHADOWED', 'HELD')),
    CONSTRAINT ck_notif_event_inbox_envelope CHECK (pg_column_size(envelope) <= 12288),
    CONSTRAINT ck_notif_event_inbox_lease CHECK ((lease_id IS NULL) = (lease_until IS NULL))
);
CREATE UNIQUE INDEX uq_notif_event_inbox_decision ON notif_event_inbox (producer, route_decision_id);
CREATE INDEX ix_notif_event_inbox_due ON notif_event_inbox (lane, received_at) WHERE state = 'PENDING';
CREATE INDEX ix_notif_event_inbox_held ON notif_event_inbox (route_category, received_at) WHERE state = 'HELD';
CREATE INDEX ix_notif_event_inbox_audit ON notif_event_inbox (route_category, received_at);

-- 2. one logical notification per recipient
CREATE TABLE notif_notifications (
    id              UUID         NOT NULL,
    recipient_ref   VARCHAR(80)  NOT NULL,
    category        VARCHAR(40)  NOT NULL,
    route_category  VARCHAR(40)  NOT NULL,
    lane            VARCHAR(10)  NOT NULL,
    dedupe_key      VARCHAR(200) NOT NULL,
    coalesce_key    VARCHAR(200),
    cancel_key      VARCHAR(200),
    source_producer VARCHAR(64)  NOT NULL,
    source_event_id VARCHAR(64)  NOT NULL,
    subject_kind    VARCHAR(32),
    subject_id      UUID,
    template_key    VARCHAR(64)  NOT NULL,
    params          JSONB        NOT NULL,
    policy_snapshot JSONB        NOT NULL,
    state           VARCHAR(24)  NOT NULL,
    deliver_after   TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_notifications PRIMARY KEY (id),
    CONSTRAINT uq_notif_notifications_dedupe UNIQUE (recipient_ref, dedupe_key),
    CONSTRAINT ck_notif_notifications_recipient CHECK (recipient_ref ~ '^(cfmeet-user|meeting-email-principal|platform-principal):[0-9a-f-]{36}$|^operator-group:[a-z][a-z-]{0,39}$'),
    CONSTRAINT ck_notif_notifications_lane CHECK (lane IN ('CRITICAL', 'HIGH', 'STANDARD')),
    CONSTRAINT ck_notif_notifications_params CHECK (pg_column_size(params) <= 2048),
    CONSTRAINT ck_notif_notifications_state CHECK (state IN ('ROUTED', 'SUPPRESSED', 'CANCELLED', 'CANCELLED_FOR_REROUTE', 'EXPIRED', 'SHADOW', 'HELD'))
);
CREATE INDEX ix_notif_notifications_cancel ON notif_notifications (cancel_key) WHERE cancel_key IS NOT NULL;
CREATE INDEX ix_notif_notifications_source ON notif_notifications (source_producer, source_event_id);
CREATE INDEX ix_notif_notifications_created ON notif_notifications (created_at);

-- 3. one delivery per (notification, channel, target)
CREATE TABLE notif_deliveries (
    id               UUID        NOT NULL,
    notification_id  UUID        NOT NULL REFERENCES notif_notifications (id) ON DELETE CASCADE,
    channel          VARCHAR(16) NOT NULL,
    lane             VARCHAR(10) NOT NULL,
    target_ref       VARCHAR(80) NOT NULL,
    status           VARCHAR(20) NOT NULL,
    attempts         INT         NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_id         UUID,
    lease_until      TIMESTAMPTZ,
    last_error_class VARCHAR(64),
    routing_version  INT         NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_notif_deliveries PRIMARY KEY (id),
    CONSTRAINT uq_notif_deliveries_target UNIQUE (notification_id, channel, target_ref),
    CONSTRAINT ck_notif_deliveries_channel CHECK (channel IN ('EMAIL', 'PUSH', 'IN_APP', 'OPERATOR', 'WEBHOOK')),
    CONSTRAINT ck_notif_deliveries_lane CHECK (lane IN ('CRITICAL', 'HIGH', 'STANDARD')),
    CONSTRAINT ck_notif_deliveries_status CHECK (status IN ('PENDING', 'DEFERRED', 'DISPATCHING', 'SENT', 'DELIVERED', 'FAILED', 'EXPIRED', 'CANCELLED', 'SUPPRESSED', 'SHADOW')),
    CONSTRAINT ck_notif_deliveries_attempts CHECK (attempts BETWEEN 0 AND 64),
    CONSTRAINT ck_notif_deliveries_lease CHECK ((status = 'DISPATCHING') = (lease_id IS NOT NULL))
);
CREATE INDEX ix_notif_deliveries_due ON notif_deliveries (lane, next_attempt_at) WHERE status IN ('PENDING', 'DEFERRED');
CREATE INDEX ix_notif_deliveries_dispatching ON notif_deliveries (lease_until) WHERE status = 'DISPATCHING';
CREATE INDEX ix_notif_deliveries_notification ON notif_deliveries (notification_id);

-- 4. append-only attempts
CREATE TABLE notif_delivery_attempts (
    delivery_id UUID        NOT NULL REFERENCES notif_deliveries (id) ON DELETE CASCADE,
    attempt     INT         NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ,
    outcome     VARCHAR(16) NOT NULL,
    error_class VARCHAR(64),
    http_status INT,
    CONSTRAINT pk_notif_delivery_attempts PRIMARY KEY (delivery_id, attempt),
    CONSTRAINT ck_notif_delivery_attempts_outcome CHECK (outcome IN ('OK', 'RETRY', 'PERMANENT', 'BREAKER_OPEN'))
);
CREATE TRIGGER notif_delivery_attempts_append_only BEFORE UPDATE ON notif_delivery_attempts
    FOR EACH ROW EXECUTE FUNCTION notif_append_only();

-- 5. the user-visible inbox
CREATE TABLE notif_inbox_items (
    id            UUID        NOT NULL,
    recipient_ref VARCHAR(80) NOT NULL,
    category      VARCHAR(40) NOT NULL,
    template_key  VARCHAR(64) NOT NULL,
    params        JSONB       NOT NULL,
    action_kind   VARCHAR(32) NOT NULL,
    action_ref    UUID,
    group_key     VARCHAR(200),
    group_count   INT         NOT NULL DEFAULT 1,
    silent        BOOLEAN     NOT NULL DEFAULT false,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at       TIMESTAMPTZ,
    dismissed_at  TIMESTAMPTZ,
    expires_at    TIMESTAMPTZ NOT NULL,
    version       BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT pk_notif_inbox_items PRIMARY KEY (id),
    CONSTRAINT ck_notif_inbox_items_params CHECK (pg_column_size(params) <= 2048),
    CONSTRAINT ck_notif_inbox_items_action CHECK (action_kind IN ('OPEN_CONVERSATION', 'OPEN_CALL_HISTORY', 'OPEN_VOICEMAIL', 'OPEN_RECORDING', 'OPEN_MEETING', 'OPEN_ORGANIZATION', 'OPEN_ORGANIZATION_INVITATION', 'OPEN_CONTACT_REQUEST', 'OPEN_CONTACT', 'OPEN_SECURITY_SETTINGS', 'OPEN_DEVELOPER_APPLICATION', 'NONE')),
    CONSTRAINT ck_notif_inbox_items_group CHECK (group_count >= 1)
);
CREATE INDEX ix_notif_inbox_recipient ON notif_inbox_items (recipient_ref, created_at DESC, id DESC) WHERE dismissed_at IS NULL;
CREATE INDEX ix_notif_inbox_unread ON notif_inbox_items (recipient_ref) WHERE read_at IS NULL AND dismissed_at IS NULL AND silent = false;
CREATE UNIQUE INDEX uq_notif_inbox_group ON notif_inbox_items (recipient_ref, group_key) WHERE group_key IS NOT NULL AND read_at IS NULL AND dismissed_at IS NULL;
CREATE INDEX ix_notif_inbox_expiry ON notif_inbox_items (expires_at);

-- 6. reminders, quiet-hour deferrals, deliver-after
CREATE TABLE notif_schedules (
    schedule_key VARCHAR(200) NOT NULL,
    kind         VARCHAR(16)  NOT NULL,
    release_at   TIMESTAMPTZ  NOT NULL,
    cancel_key   VARCHAR(200),
    payload      JSONB        NOT NULL,
    delivery_id  UUID REFERENCES notif_deliveries (id) ON DELETE CASCADE,
    state        VARCHAR(12)  NOT NULL DEFAULT 'PENDING',
    lease_id     UUID,
    lease_until  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    fired_at     TIMESTAMPTZ,
    CONSTRAINT pk_notif_schedules PRIMARY KEY (schedule_key),
    CONSTRAINT ck_notif_schedules_kind CHECK (kind IN ('REMINDER', 'QUIET_HOURS', 'DELIVER_AFTER', 'COALESCE')),
    CONSTRAINT ck_notif_schedules_state CHECK (state IN ('PENDING', 'FIRED', 'CANCELLED')),
    CONSTRAINT ck_notif_schedules_payload CHECK (pg_column_size(payload) <= 2048)
);
CREATE INDEX ix_notif_schedules_due ON notif_schedules (release_at) WHERE state = 'PENDING';
CREATE INDEX ix_notif_schedules_cancel ON notif_schedules (cancel_key) WHERE cancel_key IS NOT NULL;

-- 7. service-side routing per (category, channel)
CREATE TABLE notif_routing (
    category       VARCHAR(40) NOT NULL,
    channel        VARCHAR(16) NOT NULL,
    mode           VARCHAR(8)  NOT NULL DEFAULT 'OFF',
    fence_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    version        INT         NOT NULL DEFAULT 1,
    canary_percent SMALLINT    NOT NULL DEFAULT 0,
    reminder_boundary TIMESTAMPTZ,
    changed_by_run VARCHAR(64) NOT NULL,
    changed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_routing PRIMARY KEY (category, channel),
    CONSTRAINT ck_notif_routing_mode CHECK (mode IN ('OFF', 'SHADOW', 'ACTIVE')),
    CONSTRAINT ck_notif_routing_channel CHECK (channel IN ('EMAIL', 'PUSH', 'IN_APP', 'OPERATOR')),
    CONSTRAINT ck_notif_routing_canary CHECK (canary_percent BETWEEN 0 AND 100),
    CONSTRAINT ck_notif_routing_version CHECK (version >= 1)
);

-- 8. jti replay guard
CREATE TABLE notif_service_nonces (
    jti        VARCHAR(64) NOT NULL,
    issuer     VARCHAR(64) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_notif_service_nonces PRIMARY KEY (issuer, jti)
);
CREATE INDEX ix_notif_service_nonces_expiry ON notif_service_nonces (expires_at);

-- 9. temporary shadow comparison
CREATE TABLE notif_shadow_observations (
    id               BIGSERIAL    NOT NULL,
    category         VARCHAR(40)  NOT NULL,
    dedupe_key       VARCHAR(200) NOT NULL,
    side             VARCHAR(8)   NOT NULL,
    recipient_digest CHAR(64)     NOT NULL,
    channel          VARCHAR(16)  NOT NULL,
    decision         VARCHAR(16)  NOT NULL,
    render_digest    CHAR(64),
    observed_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_shadow_observations PRIMARY KEY (id),
    CONSTRAINT uq_notif_shadow_observations UNIQUE (category, dedupe_key, side, recipient_digest, channel),
    CONSTRAINT ck_notif_shadow_observations_side CHECK (side IN ('LEGACY', 'NS'))
);
CREATE INDEX ix_notif_shadow_observations_time ON notif_shadow_observations (observed_at);

-- 10. the email ledger
CREATE TABLE notif_email_messages (
    id                  UUID         NOT NULL,
    delivery_id         UUID REFERENCES notif_deliveries (id) ON DELETE SET NULL,
    category            VARCHAR(48)  NOT NULL,
    lane                VARCHAR(10)  NOT NULL,
    idempotency_key     VARCHAR(160) NOT NULL,
    route_decision_id   UUID,
    sender_profile      VARCHAR(32)  NOT NULL DEFAULT 'default',
    recipient_digest    CHAR(64)     NOT NULL,
    recipient_sealed    TEXT,
    subject_sealed      TEXT         NOT NULL,
    body_sealed         TEXT,
    status              VARCHAR(16)  NOT NULL DEFAULT 'QUEUED',
    provider            VARCHAR(16),
    provider_message_id VARCHAR(128),
    attempts            INT          NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    lease_holder        VARCHAR(96),
    lease_expires_at    TIMESTAMPTZ,
    last_error_class    VARCHAR(64),
    last_error_status   INT,
    expires_at          TIMESTAMPTZ  NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    accepted_at         TIMESTAMPTZ,
    delivered_at        TIMESTAMPTZ,
    final_at            TIMESTAMPTZ,
    CONSTRAINT pk_notif_email_messages PRIMARY KEY (id),
    CONSTRAINT uq_notif_email_messages_key UNIQUE (idempotency_key),
    CONSTRAINT uq_notif_email_messages_delivery UNIQUE (delivery_id),
    CONSTRAINT ck_notif_email_messages_category CHECK (category ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_notif_email_messages_lane CHECK (lane IN ('CRITICAL', 'STANDARD')),
    CONSTRAINT ck_notif_email_messages_sender CHECK (sender_profile ~ '^[a-z][a-z0-9-]{0,31}$'),
    CONSTRAINT ck_notif_email_messages_status CHECK (status IN ('QUEUED', 'SENDING', 'ACCEPTED', 'DELIVERED', 'DELAYED', 'BOUNCED', 'COMPLAINED', 'FAILED', 'EXPIRED', 'SUPPRESSED')),
    CONSTRAINT ck_notif_email_messages_lease CHECK ((status = 'SENDING') = (lease_holder IS NOT NULL))
);
CREATE UNIQUE INDEX uq_notif_email_provider_msg ON notif_email_messages (provider, provider_message_id) WHERE provider_message_id IS NOT NULL;
CREATE INDEX ix_notif_email_due_critical ON notif_email_messages (next_attempt_at) WHERE status IN ('QUEUED', 'SENDING') AND lane = 'CRITICAL';
CREATE INDEX ix_notif_email_due_standard ON notif_email_messages (next_attempt_at) WHERE status IN ('QUEUED', 'SENDING') AND lane = 'STANDARD';
CREATE INDEX ix_notif_email_final ON notif_email_messages (final_at) WHERE final_at IS NOT NULL;
CREATE INDEX ix_notif_email_recipient ON notif_email_messages (recipient_digest);
CREATE INDEX ix_notif_email_decision ON notif_email_messages (route_decision_id) WHERE route_decision_id IS NOT NULL;

-- 11. provider webhook events
CREATE TABLE notif_email_provider_events (
    provider            VARCHAR(16)  NOT NULL,
    event_id            VARCHAR(128) NOT NULL,
    event_type          VARCHAR(48)  NOT NULL,
    provider_message_id VARCHAR(128),
    message_id          UUID,
    occurred_at         TIMESTAMPTZ,
    received_at         TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_email_provider_events PRIMARY KEY (provider, event_id)
);
CREATE INDEX ix_notif_email_provider_events_unlinked ON notif_email_provider_events (provider, provider_message_id) WHERE message_id IS NULL;

-- 12. bounce / complaint suppression (never applied to the CRITICAL lane)
CREATE TABLE notif_email_suppressions (
    recipient_digest CHAR(64)    NOT NULL,
    reason           VARCHAR(16) NOT NULL,
    bounce_count     INT         NOT NULL DEFAULT 0,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_email_suppressions PRIMARY KEY (recipient_digest),
    CONSTRAINT ck_notif_email_suppressions_reason CHECK (reason IN ('COMPLAINED', 'HARD_BOUNCE', 'SOFT_BOUNCE', 'MANUAL'))
);

-- 13. push registrations (sealed handles)
CREATE TABLE notif_push_endpoints (
    id                 UUID        NOT NULL,
    tenant_id          UUID        NOT NULL,
    application_id     UUID        NOT NULL,
    environment_id     UUID        NOT NULL,
    account_ref        VARCHAR(64),
    principal_id       UUID,
    account_device_ref UUID        NOT NULL,
    platform           VARCHAR(12) NOT NULL,
    handle_sealed      TEXT        NOT NULL,
    handle_key_id      VARCHAR(32) NOT NULL,
    handle_digest      CHAR(64)    NOT NULL,
    enabled            BOOLEAN     NOT NULL DEFAULT true,
    disabled_reason    VARCHAR(48),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_push_endpoints PRIMARY KEY (id),
    CONSTRAINT uq_notif_push_endpoints_device UNIQUE (environment_id, account_device_ref, platform),
    CONSTRAINT uq_notif_push_endpoints_handle UNIQUE (handle_digest),
    CONSTRAINT ck_notif_push_endpoints_platform CHECK (platform IN ('APNS', 'APNS_VOIP', 'FCM', 'WEB_PUSH', 'LOCAL_TEST')),
    CONSTRAINT ck_notif_push_endpoints_owner CHECK (account_ref IS NOT NULL OR principal_id IS NOT NULL),
    CONSTRAINT ck_notif_push_endpoints_disabled CHECK (enabled OR disabled_reason IS NOT NULL)
);
CREATE INDEX ix_notif_push_endpoints_account ON notif_push_endpoints (account_ref) WHERE enabled;

-- 14. account preferences (absent row = everything enabled, today's Hub defaults)
CREATE TABLE notif_preferences (
    account_ref         VARCHAR(64) NOT NULL,
    version             BIGINT      NOT NULL DEFAULT 1,
    direct_messages     BOOLEAN     NOT NULL DEFAULT true,
    group_messages      BOOLEAN     NOT NULL DEFAULT true,
    incoming_calls      BOOLEAN     NOT NULL DEFAULT true,
    missed_calls        BOOLEAN     NOT NULL DEFAULT true,
    contact_requests    BOOLEAN     NOT NULL DEFAULT true,
    group_invitations   BOOLEAN     NOT NULL DEFAULT true,
    meeting_invitations BOOLEAN     NOT NULL DEFAULT true,
    meeting_reminders   BOOLEAN     NOT NULL DEFAULT true,
    voicemail           BOOLEAN     NOT NULL DEFAULT true,
    organization_updates BOOLEAN    NOT NULL DEFAULT true,
    developer_updates   BOOLEAN     NOT NULL DEFAULT true,
    channel_overrides   JSONB       NOT NULL DEFAULT '{}'::jsonb,
    quiet_hours         JSONB,
    source_version      BIGINT,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_preferences PRIMARY KEY (account_ref),
    CONSTRAINT ck_notif_preferences_overrides CHECK (jsonb_typeof(channel_overrides) = 'object' AND pg_column_size(channel_overrides) <= 2048),
    CONSTRAINT ck_notif_preferences_version CHECK (version >= 0)
);

-- 15. conversation notification level
CREATE TABLE notif_conversation_preferences (
    account_ref     VARCHAR(64) NOT NULL,
    conversation_id UUID        NOT NULL,
    notifications   VARCHAR(8)  NOT NULL DEFAULT 'ALL',
    muted_until     TIMESTAMPTZ,
    version         BIGINT      NOT NULL DEFAULT 1,
    source_version  BIGINT,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_conversation_preferences PRIMARY KEY (account_ref, conversation_id),
    CONSTRAINT ck_notif_conversation_preferences_level CHECK (notifications IN ('ALL', 'MENTIONS', 'MUTED')),
    CONSTRAINT ck_notif_conversation_preferences_muted CHECK (muted_until IS NULL OR notifications = 'MUTED')
);

-- 16. contact notification level
CREATE TABLE notif_contact_preferences (
    account_ref    VARCHAR(64) NOT NULL,
    contact_ref    VARCHAR(64) NOT NULL,
    notifications  VARCHAR(8)  NOT NULL DEFAULT 'ALL',
    version        BIGINT      NOT NULL DEFAULT 1,
    source_version BIGINT,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_contact_preferences PRIMARY KEY (account_ref, contact_ref),
    CONSTRAINT ck_notif_contact_preferences_level CHECK (notifications IN ('ALL', 'MENTIONS', 'MUTED'))
);

-- 17. 7-day preference write-back log
CREATE TABLE notif_preference_change_log (
    account_ref VARCHAR(64) NOT NULL,
    scope       VARCHAR(16) NOT NULL,
    scope_ref   VARCHAR(64) NOT NULL DEFAULT '',
    changed_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    new_value   JSONB       NOT NULL,
    version     BIGINT      NOT NULL,
    CONSTRAINT pk_notif_preference_change_log PRIMARY KEY (account_ref, scope, scope_ref, changed_at),
    CONSTRAINT ck_notif_preference_change_log_scope CHECK (scope IN ('ACCOUNT', 'CONVERSATION', 'CONTACT')),
    CONSTRAINT ck_notif_preference_change_log_value CHECK (pg_column_size(new_value) <= 2048)
);
CREATE INDEX ix_notif_preference_change_log_time ON notif_preference_change_log (changed_at);

-- Dark deployment: every (category, channel) the service can ever deliver starts OFF.
INSERT INTO notif_routing (category, channel, mode, changed_by_run) VALUES
    ('ORGANIZATION_UPDATES', 'IN_APP', 'OFF', 'migration'),
    ('ORGANIZATION_UPDATES', 'PUSH', 'OFF', 'migration'),
    ('CONTACT_REQUESTS', 'IN_APP', 'OFF', 'migration'),
    ('CONTACT_REQUESTS', 'PUSH', 'OFF', 'migration'),
    ('MESSAGE_WAKEUPS', 'PUSH', 'OFF', 'migration'),
    ('GROUP_INVITATIONS', 'IN_APP', 'OFF', 'migration'),
    ('GROUP_INVITATIONS', 'PUSH', 'OFF', 'migration'),
    ('MISSED_CALLS', 'IN_APP', 'OFF', 'migration'),
    ('MISSED_CALLS', 'PUSH', 'OFF', 'migration'),
    ('VOICEMAIL', 'IN_APP', 'OFF', 'migration'),
    ('VOICEMAIL', 'PUSH', 'OFF', 'migration'),
    ('MEETING_NOTIFICATION', 'EMAIL', 'OFF', 'migration'),
    ('MEETING_NOTIFICATION', 'IN_APP', 'OFF', 'migration'),
    ('MEETING_NOTIFICATION', 'PUSH', 'OFF', 'migration'),
    ('MEETING_INVITATION', 'EMAIL', 'OFF', 'migration'),
    ('ORGANIZATION_INVITATION', 'EMAIL', 'OFF', 'migration'),
    ('AUTH_VERIFICATION', 'EMAIL', 'OFF', 'migration'),
    ('AUTH_PASSWORD_RESET', 'EMAIL', 'OFF', 'migration'),
    ('SECURITY_DEVICE_STEP_UP', 'EMAIL', 'OFF', 'migration'),
    ('SECURITY_PASSWORD_CHANGED', 'EMAIL', 'OFF', 'migration'),
    ('SECURITY_PASSWORD_CHANGED', 'IN_APP', 'OFF', 'migration'),
    ('SECURITY_DEVICE_LINKED', 'EMAIL', 'OFF', 'migration'),
    ('SECURITY_DEVICE_LINKED', 'IN_APP', 'OFF', 'migration'),
    ('DEVELOPER_PLATFORM', 'EMAIL', 'OFF', 'migration'),
    ('DEVELOPER_PLATFORM', 'IN_APP', 'OFF', 'migration'),
    ('OPERATOR_ALERTS', 'OPERATOR', 'OFF', 'migration'),
    ('CALL_INVITATIONS', 'PUSH', 'OFF', 'migration');
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notif_event_inbox) OR EXISTS (SELECT 1 FROM notif_email_messages)
       OR EXISTS (SELECT 1 FROM notif_preferences) OR EXISTS (SELECT 1 FROM notif_push_endpoints) THEN
        RAISE EXCEPTION 'notifications foundation holds data: refusing to downgrade';
    END IF;
END $$;
DROP TABLE notif_preference_change_log;
DROP TABLE notif_contact_preferences;
DROP TABLE notif_conversation_preferences;
DROP TABLE notif_preferences;
DROP TABLE notif_push_endpoints;
DROP TABLE notif_email_suppressions;
DROP TABLE notif_email_provider_events;
DROP TABLE notif_email_messages;
DROP TABLE notif_shadow_observations;
DROP TABLE notif_service_nonces;
DROP TABLE notif_routing;
DROP TABLE notif_schedules;
DROP TABLE notif_inbox_items;
DROP TABLE notif_delivery_attempts;
DROP TABLE notif_deliveries;
DROP TABLE notif_notifications;
DROP TABLE notif_event_inbox;
DROP FUNCTION notif_append_only();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
