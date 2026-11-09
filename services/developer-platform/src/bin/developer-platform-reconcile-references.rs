//! DATA-017 — report-only reconciliation of the foreign references held by the Developer
//! Platform.
//!
//! `DATABASE_URL=… cargo run --bin developer-platform-reconcile-references > report.json`
//!
//! Local checks run in one REPEATABLE READ, READ ONLY transaction. With the worker's
//! service authentication configured (`CF_PLATFORM_ACCESS_BASE_URL` and the `CF_*`
//! identity), each organization / project / environment is also verified at Platform
//! Access (read-only delegated reads, at most
//! `DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT` of each, default 1000).
//! Nothing is written, repaired or deleted. Prints the report JSON on stdout and a
//! one-line summary on stderr. Exit status: 0 consistent, 1 anomalies found (report
//! only), 2-4 usage / connection / query failures.

use developer_platform::{
    config::service_auth::ServiceAuthSettings,
    platform_access::{HttpPlatformAccess, PlatformAccessPort},
    reference_reconciliation::full_report,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL is required");
        std::process::exit(2);
    };
    let limit = match std::env::var("DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT") {
        Err(_) => 1_000,
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(n) if n <= 100_000 => n,
            _ => {
                eprintln!(
                    "DEVELOPER_PLATFORM_REFERENCE_RECONCILIATION_REMOTE_LIMIT must be 0-100000"
                );
                std::process::exit(2);
            }
        },
    };
    let db = match sea_orm::Database::connect(url).await {
        Ok(db) => db,
        Err(_) => {
            eprintln!("database connection failed");
            std::process::exit(3);
        }
    };
    let platform: Option<HttpPlatformAccess> = match ServiceAuthSettings::from_env() {
        Ok(s) if s.has_platform_access() && limit > 0 => {
            match s.load(std::sync::Arc::new(cf_service_auth::SystemClock)) {
                Ok(runtime) => Some(HttpPlatformAccess::new(runtime.client())),
                Err(e) => {
                    eprintln!("service authentication configuration error: {e}");
                    std::process::exit(2);
                }
            }
        }
        _ => None,
    };
    let platform_ref = platform.as_ref().map(|p| p as &dyn PlatformAccessPort);
    match full_report(&db, platform_ref, limit).await {
        Ok(r) => {
            eprintln!(
                "reference checks={} anomalies={} remote={}",
                r.checks.len(),
                r.anomalies,
                r.remote_verification
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
            eprintln!(
                "reconciliation failed: {}",
                developer_platform::db::summarize_db_error(&e)
            );
            std::process::exit(4);
        }
    }
}
