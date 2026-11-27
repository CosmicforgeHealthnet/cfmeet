//! ADR 0069 — read-only export of the retired Meeting Service channels for the Messaging
//! import (`messaging-admin social import-channels`). Statuses are not exported (24 h).
//!
//! ```text
//! DATABASE_URL=… CFMEET_COMMUNICATION_TENANT_ID=… CFMEET_COMMUNICATION_APPLICATION_ID=… \
//! CFMEET_COMMUNICATION_ENVIRONMENT_ID=… export-social-channels > social-channels.json
//! ```
//!
//! One REPEATABLE READ, READ ONLY transaction: consistent and never written. Prints the
//! document on stdout and only counts and the checksum on stderr (never content).
use backend::services::social_export::{export, ExportError, ExportScope};
use uuid::Uuid;

fn uuid_var(name: &str) -> Option<Uuid> {
    std::env::var(name)
        .ok()
        .and_then(|v| Uuid::parse_str(v.trim()).ok())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL is required");
        std::process::exit(2);
    };
    let (Some(tenant_id), Some(application_id), Some(environment_id)) = (
        uuid_var("CFMEET_COMMUNICATION_TENANT_ID"),
        uuid_var("CFMEET_COMMUNICATION_APPLICATION_ID"),
        uuid_var("CFMEET_COMMUNICATION_ENVIRONMENT_ID"),
    ) else {
        eprintln!("CFMEET_COMMUNICATION_TENANT_ID, _APPLICATION_ID and _ENVIRONMENT_ID are required (UUIDs)");
        std::process::exit(2);
    };
    let db = match sea_orm::Database::connect(url).await {
        Ok(db) => db,
        Err(_) => {
            eprintln!("database connection failed");
            std::process::exit(3);
        }
    };
    let scope = ExportScope {
        tenant_id,
        application_id,
        environment_id,
    };
    match export(&db, scope).await {
        Ok(doc) => {
            eprintln!(
                "exported channels={} followers={} posts={} reactions={} media={} skipped={} checksum={}",
                doc.counts.channels,
                doc.counts.followers,
                doc.counts.posts,
                doc.counts.reactions,
                doc.counts.media,
                serde_json::to_string(&doc.skipped).unwrap_or_default(),
                doc.checksum
            );
            println!(
                "{}",
                serde_json::to_string(&doc).expect("export serializes")
            );
        }
        Err(ExportError::Database(e)) => {
            eprintln!("export failed: {e}");
            std::process::exit(4);
        }
    }
}
