//! Blueprint Section 4 — read-only export of the legacy first-party organizations for the
//! Organizations Service import (`docs/api/organizations-internal-v1.md` §5).
//!
//! `DATABASE_URL=… cargo run --bin export_organizations > organizations-export.json`
//!
//! One REPEATABLE READ, READ ONLY transaction: the snapshot is consistent and the source is
//! never written. Prints the export JSON on stdout and a count/checksum summary on stderr.
use backend::services::organization_export::{export, ExportError};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL is required");
        std::process::exit(2);
    };
    let db = match sea_orm::Database::connect(url).await {
        Ok(db) => db,
        Err(_) => {
            eprintln!("database connection failed");
            std::process::exit(3);
        }
    };
    match export(&db).await {
        Ok(document) => {
            eprintln!(
                "exported organizations={} memberships={} checksum={}",
                document.counts.organizations, document.counts.memberships, document.checksum
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&document).expect("export serializes")
            );
        }
        Err(ExportError::Database(e)) => {
            eprintln!("export failed: {e}");
            std::process::exit(4);
        }
    }
}
