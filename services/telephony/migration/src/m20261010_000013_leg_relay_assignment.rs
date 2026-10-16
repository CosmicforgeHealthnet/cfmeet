//! Regulatory identity evidence (Part H §78, §84): `telephony_legs.relay_assignment_ref`
//! is the Privacy Relay assignment (opaque reference) of the caller identity an outbound
//! leg presented — evidence of WHICH platform relay identity was lent to the session for
//! this execution, without storing the number. Set once at route choice, immutable after.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE telephony_legs ADD COLUMN relay_assignment_ref UUID;
DROP TRIGGER trg_telephony_legs_set_once ON telephony_legs;
CREATE TRIGGER trg_telephony_legs_set_once
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'route_id', 'provider_id', 'destination_country', 'provider_cost_minor', 'provider_cost_currency',
        'originating_at', 'ringing_at',
        'answered_at', 'connected_at', 'ended_at', 'end_reason', 'failure_category', 'relay_assignment_ref');
"#;

const DOWN: &str = r#"
DROP TRIGGER trg_telephony_legs_set_once ON telephony_legs;
CREATE TRIGGER trg_telephony_legs_set_once
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'route_id', 'provider_id', 'destination_country', 'provider_cost_minor', 'provider_cost_currency',
        'originating_at', 'ringing_at',
        'answered_at', 'connected_at', 'ended_at', 'end_reason', 'failure_category');
ALTER TABLE telephony_legs DROP COLUMN relay_assignment_ref;
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
