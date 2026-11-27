//! Meeting audit-closure history (calendar feeds, audit MF2): `up` applies it; other
//! subcommands use the standard CLI against the same separate history table
//! (`meeting_audit_closure_migrations`). Kept apart from every earlier history so the
//! previous image, which fails closed on versions it does not know, still starts after a
//! rollback.

#[async_std::main]
async fn main() {
    // SEC-015: refuse down / fresh / refresh / reset outside a non-production APP_ENV
    // unless CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand. Runs before any .env load.
    cf_migration_guard::enforce_from_env();
    sea_orm_migration::cli::run_cli(migration::MeetingAuditMigrator).await;
}
