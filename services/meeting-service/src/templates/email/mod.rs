mod account;
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
