//! Account-security history (Blueprint v1.0 Section 2): `up` applies it; other
//! subcommands use the standard CLI against the same separate history table.
#[async_std::main]
async fn main() {
    sea_orm_migration::cli::run_cli(migration::AccountSecurityMigrator).await;
}
