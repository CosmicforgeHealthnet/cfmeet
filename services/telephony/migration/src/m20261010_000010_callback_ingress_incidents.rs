//! Invalid-callback and SIP ingress incident aggregation (Part H §59–§60).
//!
//! * `telephony_ingress_windows.subject_kind` gains the fixed-window counters of
//!   malformed / oversized callbacks, callbacks for unknown providers, callback replays
//!   and rejected SIP ingress (per peer).
//! * `telephony_incidents.category` gains the aggregated incidents those windows raise
//!   once their per-minute threshold is crossed: `CALLBACK_MALFORMED`,
//!   `CALLBACK_UNKNOWN_PROVIDER`, `CALLBACK_REPLAY_ABUSE`, `SIP_INGRESS_RATE_LIMITED`.
//!
//! The window subject stays a provider / peer id or the literal `unknown` — never an
//! attacker-supplied identifier.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE telephony_ingress_windows
    DROP CONSTRAINT ck_telephony_ingress_windows_kind,
    ADD CONSTRAINT ck_telephony_ingress_windows_kind CHECK (subject_kind IN (
        'CALLBACK_AUTH_FAILURE', 'CALLBACK_ACCEPTED', 'SIP_INGRESS',
        'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY', 'SIP_INGRESS_REJECTED'));

ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED', 'CAPTURE_DELIVERY_DEAD',
        'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY_ABUSE', 'SIP_INGRESS_RATE_LIMITED'));
"#;

const DOWN: &str = r#"
DELETE FROM telephony_incidents WHERE category IN (
    'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY_ABUSE', 'SIP_INGRESS_RATE_LIMITED');
ALTER TABLE telephony_incidents
    DROP CONSTRAINT ck_telephony_incidents_category,
    ADD CONSTRAINT ck_telephony_incidents_category CHECK (category IN (
        'UNKNOWN_ORIGINATION_OUTCOME', 'UNKNOWN_TERMINATION_OUTCOME', 'PROVIDER_ACTIVE_WITHOUT_BINDING',
        'CONNECTED_BUT_PROVIDER_ENDED', 'ORPHAN_BRIDGE_LEG', 'RINGING_TIMEOUT', 'PROVIDER_EVENT_GAP',
        'CALLBACK_AUTH_FAILURES', 'UNKNOWN_PROVIDER_OBJECT', 'LATE_EVENT_AFTER_TERMINAL',
        'MISSING_USAGE', 'DURATION_LIMIT_ENFORCED', 'PROVIDER_CIRCUIT_OPENED', 'SIP_PEER_REJECTED', 'UNKNOWN_CONTROL_OUTCOME',
        'ABUSE_SIGNAL',
        'ORPHAN_ENTERPRISE_SIP_LEG', 'ENTERPRISE_DEVICE_REJECTED', 'CAPTURE_DELIVERY_DEAD'));
DELETE FROM telephony_ingress_windows WHERE subject_kind IN (
    'CALLBACK_MALFORMED', 'CALLBACK_UNKNOWN_PROVIDER', 'CALLBACK_REPLAY', 'SIP_INGRESS_REJECTED');
ALTER TABLE telephony_ingress_windows
    DROP CONSTRAINT ck_telephony_ingress_windows_kind,
    ADD CONSTRAINT ck_telephony_ingress_windows_kind
        CHECK (subject_kind IN ('CALLBACK_AUTH_FAILURE', 'CALLBACK_ACCEPTED', 'SIP_INGRESS'));
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
