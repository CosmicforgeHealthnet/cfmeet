//! Partner push (ADR 0063, phase B5; contract `cfmeet.notifications.internal-api` 1.3.0):
//! additive only.
//!
//! * `notif_push_providers` — N-P1, the provider vault: one partner APNs, APNs VoIP or FCM
//!   credential per (tenant, application, environment, provider), stored **sealed** under
//!   the service's data keys and bound (AEAD associated data) to exactly that row's scope
//!   and provider, so a sealed secret copied to another scope never opens. The key is the
//!   scope: a credential is only ever paired with endpoints of the same (tenant,
//!   application, environment), and an environment holds at most one credential per
//!   provider (`UNIQUE (environment_id, provider)`), so it can never be shared by two
//!   tenants or applications. Only a `v2.` seal can be stored (never plaintext).
//! * `notif_push_endpoints` gains `subject_kind` / `installation_id` — N-P2, partner
//!   registrations keyed by (scope, principal, installation, platform); a partner row has
//!   no CF Meet account, is APNS / APNS_VOIP / FCM only and never in the first-party scope.
//! * `notif_notifications.recipient_ref` widens to 120 characters and accepts
//!   `platform-principal:<environmentId>:<principalId>` (N-P4).
//!
//! `down` refuses while any partner credential, registration or notification exists.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE notif_push_providers (
    tenant_id        UUID        NOT NULL,
    application_id   UUID        NOT NULL,
    environment_id   UUID        NOT NULL,
    provider         VARCHAR(12) NOT NULL,
    secret_sealed    TEXT        NOT NULL,
    secret_key_id    VARCHAR(32) NOT NULL,
    fingerprint      CHAR(64)    NOT NULL,
    metadata         JSONB       NOT NULL DEFAULT '{}'::jsonb,
    status           VARCHAR(12) NOT NULL DEFAULT 'UNVERIFIED',
    last_error       VARCHAR(48),
    last_verified_at TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_push_providers PRIMARY KEY (tenant_id, application_id, environment_id, provider),
    CONSTRAINT uq_notif_push_providers_environment UNIQUE (environment_id, provider),
    CONSTRAINT ck_notif_push_providers_provider CHECK (provider IN ('APNS', 'APNS_VOIP', 'FCM')),
    CONSTRAINT ck_notif_push_providers_status CHECK (status IN ('UNVERIFIED', 'VERIFIED', 'FAILED')),
    CONSTRAINT ck_notif_push_providers_fingerprint CHECK (fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_notif_push_providers_scope CHECK (environment_id <> '00000000-0000-0000-0000-000000000000'),
    CONSTRAINT ck_notif_push_providers_sealed CHECK (secret_sealed LIKE 'v2.%'),
    CONSTRAINT ck_notif_push_providers_metadata CHECK (jsonb_typeof(metadata) = 'object' AND pg_column_size(metadata) <= 1024)
);

ALTER TABLE notif_push_endpoints
    ADD COLUMN subject_kind    VARCHAR(24),
    ADD COLUMN installation_id VARCHAR(128);
ALTER TABLE notif_push_endpoints ADD CONSTRAINT ck_notif_push_endpoints_partner CHECK (
    subject_kind IS NULL OR (
        subject_kind = 'platform-principal'
        AND principal_id IS NOT NULL
        AND installation_id IS NOT NULL
        AND account_ref IS NULL
        AND platform IN ('APNS', 'APNS_VOIP', 'FCM')
        AND environment_id <> '00000000-0000-0000-0000-000000000000'
    )
);
CREATE UNIQUE INDEX uq_notif_push_endpoints_partner ON notif_push_endpoints
    (tenant_id, application_id, environment_id, principal_id, installation_id, platform)
    WHERE subject_kind = 'platform-principal';
CREATE INDEX ix_notif_push_endpoints_partner_principal ON notif_push_endpoints (environment_id, principal_id)
    WHERE subject_kind = 'platform-principal' AND enabled;

ALTER TABLE notif_notifications ALTER COLUMN recipient_ref TYPE VARCHAR(120);
ALTER TABLE notif_notifications DROP CONSTRAINT ck_notif_notifications_recipient;
ALTER TABLE notif_notifications ADD CONSTRAINT ck_notif_notifications_recipient CHECK (
    recipient_ref ~ '^(cfmeet-user|meeting-email-principal|platform-principal):[0-9a-f-]{36}$|^operator-group:[a-z][a-z-]{0,39}$|^platform-principal:[0-9a-f-]{36}:[0-9a-f-]{36}$'
);
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM notif_push_providers)
       OR EXISTS (SELECT 1 FROM notif_push_endpoints WHERE subject_kind IS NOT NULL)
       OR EXISTS (SELECT 1 FROM notif_notifications WHERE recipient_ref ~ '^platform-principal:[0-9a-f-]{36}:') THEN
        RAISE EXCEPTION 'partner push holds data: refusing to downgrade';
    END IF;
END $$;
ALTER TABLE notif_notifications DROP CONSTRAINT ck_notif_notifications_recipient;
ALTER TABLE notif_notifications ADD CONSTRAINT ck_notif_notifications_recipient CHECK (
    recipient_ref ~ '^(cfmeet-user|meeting-email-principal|platform-principal):[0-9a-f-]{36}$|^operator-group:[a-z][a-z-]{0,39}$'
);
ALTER TABLE notif_notifications ALTER COLUMN recipient_ref TYPE VARCHAR(80);
DROP INDEX ix_notif_push_endpoints_partner_principal;
DROP INDEX uq_notif_push_endpoints_partner;
ALTER TABLE notif_push_endpoints DROP CONSTRAINT ck_notif_push_endpoints_partner;
ALTER TABLE notif_push_endpoints DROP COLUMN installation_id, DROP COLUMN subject_kind;
DROP TABLE notif_push_providers;
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
