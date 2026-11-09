//! DATA-017 — report-only reconciliation of the cross-service ids held by Numbering.
//!
//! `DATABASE_URL=… cargo run --bin numbering-reconcile-references > references-report.json`
//!
//! One REPEATABLE READ, READ ONLY transaction; nothing is written or repaired. Prints the
//! report JSON on stdout and a one-line summary on stderr. Exit status: 0 consistent,
//! 1 anomalies found (report only), 2-4 usage / connection / query failures.
use numbering::reference_reconciliation::report;

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
    match report(&db).await {
        Ok(r) => {
            eprintln!(
                "reference checks={} anomalies={}",
                r.checks.len(),
                r.anomalies
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&r).expect("report serializes")
            );
            if r.anomalies > 0 {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("reconciliation failed: {e}");
            std::process::exit(4);
        }
    }
}
