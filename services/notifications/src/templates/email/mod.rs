//! Email templates ported verbatim from the Meeting Service (`services/meeting-service/
//! src/templates/email`, Document 4 §14.1, roadmap N3 "old-template parity"): the same
//! layout, escaping and wording, so the subject/body digest of a message rendered here
//! equals the Meeting output for the same input (`tests/templates.rs`).

pub mod account;
pub mod base;
pub mod meeting;

pub use account::{
    notification_email, password_changed_email, password_reset_email, verification_email,
    welcome_email,
};

/// Email template output containing HTML and plain text versions
pub struct EmailTemplate {
    pub html: String,
    pub text: String,
}
