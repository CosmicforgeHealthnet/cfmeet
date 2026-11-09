//! `organizations-import <export.json>` — the operator CLI equivalent of
//! `POST /internal/v1/migration/import` (contract §5), using `DATABASE_URL` (the
//! Organizations database; migrations must be applied). Prints the import report as JSON.
//!
//! `organizations-import --reconciliation` prints `GET /internal/v1/migration/reconciliation`.
//!
//! Exit status: 0 imported (conflicts, if any, are listed in the report), 1 export
//! refused, 2 usage / configuration / database error.

use organizations::{
    import::{self, Export, ImportError},
    store::{ActorKind, OpContext},
};

const USAGE: &str = "usage: organizations-import <export.json> | --reconciliation";

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 || args[0] == "--help" || args[0] == "-h" {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => {
            eprintln!("organizations-import: DATABASE_URL is not set");
            std::process::exit(2);
        }
    };
    let db = match organizations::connect(&url, 2).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("organizations-import: {e}");
            std::process::exit(2);
        }
    };
    if args[0] == "--reconciliation" {
        match import::reconciliation(&db).await {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
            Err(e) => {
                eprintln!(
                    "organizations-import: {}",
                    organizations::db::summarize_db_error(&e)
                );
                std::process::exit(2);
            }
        }
        return;
    }
    let text = match std::fs::read_to_string(&args[0]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "organizations-import: cannot read the export file ({})",
                e.kind()
            );
            std::process::exit(2);
        }
    };
    let export: Export = match serde_json::from_str(&text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "organizations-import: export refused: not a valid export document ({})",
                e.classify_name()
            );
            std::process::exit(1);
        }
    };
    let ctx = OpContext {
        kind: ActorKind::Operator,
        user_id: None,
        service: "cfmeet.organizations".into(),
        request_id: None,
        correlation_id: None,
    };
    match import::import(&db, &ctx, &export).await {
        Ok(report) => println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        ),
        Err(ImportError::Invalid(reason)) => {
            eprintln!("organizations-import: export refused: {reason}");
            std::process::exit(1);
        }
        Err(e @ ImportError::Db(_)) => {
            eprintln!("organizations-import: {e}");
            std::process::exit(2);
        }
    }
}

trait ClassifyName {
    fn classify_name(&self) -> &'static str;
}

impl ClassifyName for serde_json::Error {
    fn classify_name(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        }
    }
}
