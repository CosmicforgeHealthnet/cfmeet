use super::base::{wrap_html, wrap_text};
use super::EmailTemplate;

pub fn notification_email(username: &str, subject: &str, message: &str) -> EmailTemplate {
    let html_content = format!(
        r#"<p style="margin: 0 0 20px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    Hi <strong>{username}</strong>,
</p>
<p style="margin: 0; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    {message}
</p>"#,
        username = username,
        message = message
    );

    let text_content = format!(
        r#"{subject}

Hi {username},

{message}"#,
        subject = subject,
        username = username,
        message = message
    );

    EmailTemplate {
        html: wrap_html(subject, &html_content),
        text: wrap_text(&text_content),
    }
}
