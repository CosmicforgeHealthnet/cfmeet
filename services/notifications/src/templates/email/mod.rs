//! Email templates ported verbatim from the Meeting Service (`services/meeting-service/
//! src/templates/email`, Document 4 §14.1, roadmap N3 "old-template parity"): the same
//! layout, escaping and wording, so the subject/body digest of a message rendered here
//! equals the Meeting output for the same input (`tests/templates.rs`).

pub mod account;
pub mod base;
pub mod meeting;

pub use account::{
    email_change_code_email, email_changed_new_address_email, email_changed_old_address_email,
    email_in_use_notice_email, new_sign_in_email, notification_email, password_changed_email,
    password_reset_email, sign_in_code_email, step_up_code_email, verification_email,
    welcome_email, RequestDetails,
};

/// Email template output containing HTML and plain text versions
pub struct EmailTemplate {
    pub html: String,
    pub text: String,
}
