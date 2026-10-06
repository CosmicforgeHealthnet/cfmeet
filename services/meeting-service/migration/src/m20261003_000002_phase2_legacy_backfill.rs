//! Phase 2 — idempotent legacy → v2 backfill (spec v1.1 §B15 steps 2–4).
//!
//! Additive only: legacy `meetings` / `participants` rows are read, never changed.
//! Safe to rerun (see `phase2_backfill`). `down` is a no-op: the v2 rows are
//! removed together with the schema by the previous migration's `down`.

use sea_orm_migration::prelude::*;

use crate::phase2_backfill;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let report = phase2_backfill::run(manager.get_connection(), None).await?;
        println!("phase2 backfill: {report:?}");
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
