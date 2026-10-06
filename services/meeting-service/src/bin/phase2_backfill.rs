//! Phase 2 legacy → v2 backfill / migration evidence.
//!
//! ```text
//! cargo run --bin phase2_backfill            # run (idempotent) and print evidence
//! cargo run --bin phase2_backfill -- --dry   # evidence only, no writes
//! ```

use migration::phase2_backfill;
use sea_orm::{Database, TransactionTrait};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?;
    let dry = std::env::args().any(|a| a == "--dry");
    let db = Database::connect(&url).await?;
    let report = if dry {
        phase2_backfill::evidence(&db, None).await?
    } else {
        let txn = db.begin().await?;
        let r = phase2_backfill::run(&txn, None).await?;
        txn.commit().await?;
        r
    };
    println!(
        "CF Meet Phase 2 backfill evidence ({})",
        if dry { "dry run" } else { "applied" }
    );
    println!(
        "  legacy meeting rows ............ {}",
        report.legacy_meeting_rows
    );
    println!(
        "  meeting-space rows (mapped) .... {}",
        report.meeting_space_rows
    );
    println!(
        "  mapping success ................ {}",
        report.mapped_spaces
    );
    println!(
        "  mapping failures ............... {}",
        report.mapping_failures
    );
    println!(
        "  conference sessions (legacy) ... {}",
        report.legacy_conference_sessions
    );
    println!(
        "  legacy participant rows ........ {}",
        report.legacy_participant_rows
    );
    println!(
        "  legacy rows mapped ............. {}",
        report.mapped_legacy_participant_rows
    );
    println!(
        "  conference participants ........ {}",
        report.conference_participant_rows_from_legacy
    );
    println!(
        "  participant sessions ........... {}",
        report.participant_session_rows_from_legacy
    );
    println!(
        "  inserted this run .............. spaces={} conferences={} participants={} sessions={} refreshed={}",
        report.inserted_spaces,
        report.inserted_conferences,
        report.inserted_participants,
        report.inserted_sessions,
        report.refreshed_rows
    );
    if !report.unmapped_meeting_ids.is_empty() {
        println!("  EXCEPTIONS meetings: {:?}", report.unmapped_meeting_ids);
    }
    if !report.unmapped_participant_ids.is_empty() {
        println!(
            "  EXCEPTIONS participants: {:?}",
            report.unmapped_participant_ids
        );
    }
    if !report.is_clean() {
        std::process::exit(2);
    }
    Ok(())
}
