mod account;
pub mod base;
pub mod meeting;

pub use account::{
    data_export_ready_email, email_change_code_email, email_changed_new_address_email,
    email_changed_old_address_email, email_in_use_notice_email, new_sign_in_email,
    notification_email, security_notice_email, sign_in_code_email, step_up_code_email,
    welcome_email, RequestDetails,
};

/// Email template output containing HTML and plain text versions
pub struct EmailTemplate {
    pub html: String,
    pub text: String,
}
