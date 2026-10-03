mod base;
mod notification;
mod password_reset;
mod verification;
mod welcome;

pub use notification::notification_email;
pub use password_reset::password_reset_email;
pub use verification::verification_email;
pub use welcome::welcome_email;

/// Email template output containing HTML and plain text versions
pub struct EmailTemplate {
    pub html: String,
    pub text: String,
}
