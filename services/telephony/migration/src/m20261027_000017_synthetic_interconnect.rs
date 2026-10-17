//! Part IV Workstream L closure: synthetic TEST interconnects.
//!
//! `telecom_routes.interconnect_class` — a route is a real public interconnect (`PUBLIC`,
//! the default for every existing route) or a local synthetic test interconnect
//! (`SYNTHETIC_TEST`: the loopback SIP edge with SIPp as the carrier). A synthetic route
//! never costs anything (`estimated_leg_cost_minor = 0`), its class is fixed at creation
//! (immutable), and the service refuses to start with one in staging / production. Only
//! synthetic DCNs (Numbering's SYNTHETIC inventory) travel over it, and only while
//! `TELEPHONY_SYNTHETIC_INTERCONNECT_ENABLED` is on in a local / test runtime.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE telecom_routes
    ADD COLUMN interconnect_class VARCHAR(16) NOT NULL DEFAULT 'PUBLIC',
    ADD CONSTRAINT ck_telecom_routes_interconnect_class CHECK (
        interconnect_class IN ('PUBLIC', 'SYNTHETIC_TEST')
        AND (interconnect_class = 'PUBLIC' OR estimated_leg_cost_minor = 0));
DROP TRIGGER trg_telecom_routes_immutable ON telecom_routes;
CREATE TRIGGER trg_telecom_routes_immutable
    BEFORE UPDATE ON telecom_routes
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'provider_id', 'created_at', 'interconnect_class');
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM telecom_routes WHERE interconnect_class <> 'PUBLIC') THEN
        RAISE EXCEPTION 'synthetic test routes exist: remove them before rolling back (lossy rollback refused)'
            USING ERRCODE = '55000';
    END IF;
END $$;
DROP TRIGGER trg_telecom_routes_immutable ON telecom_routes;
CREATE TRIGGER trg_telecom_routes_immutable
    BEFORE UPDATE ON telecom_routes
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change('id', 'provider_id', 'created_at');
ALTER TABLE telecom_routes
    DROP CONSTRAINT ck_telecom_routes_interconnect_class,
    DROP COLUMN interconnect_class;
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
