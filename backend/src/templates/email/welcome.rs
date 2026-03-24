use super::base::{button, wrap_html, wrap_text};
use super::EmailTemplate;

pub fn welcome_email(username: &str, app_url: &str) -> EmailTemplate {
    let html_content = format!(
        r#"<h2 style="margin: 0 0 20px; color: #18181b; font-size: 24px; font-weight: 600;">Welcome Aboard!</h2>
<p style="margin: 0 0 20px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    Hi <strong>{username}</strong>,
</p>
<p style="margin: 0 0 30px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    Your email has been verified and your account is now active. You're all set to start using CosmicForge!
</p>
{button}"#,
        username = username,
        button = button("Get Started", app_url)
    );

    let text_content = format!(
        r#"Welcome to CosmicForge!

Hi {username},

Your email has been verified and your account is now active. You're all set to start using CosmicForge!

Get started: {app_url}"#,
        username = username,
        app_url = app_url
    );

    EmailTemplate {
        html: wrap_html("Welcome to CosmicForge", &html_content),
        text: wrap_text(&text_content),
    }
}
