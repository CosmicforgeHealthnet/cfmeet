//! Email Worker — delivers the durable email ledger through the configured provider.
//! The implementation lives with the ledger (`services::email_delivery`).

pub use crate::services::email_delivery::{spawn_email_worker, EmailWorker, EmailWorkerHandle};
