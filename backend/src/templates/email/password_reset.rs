use super::base::{wrap_html, wrap_text};
use super::EmailTemplate;

pub fn password_reset_email(username: &str, reset_code: &str) -> EmailTemplate {
    let html_content = format!(
        r#"<h2 style="margin: 0 0 20px; color: #18181b; font-size: 24px; font-weight: 600;">Reset Your Password</h2>
<p style="margin: 0 0 20px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    Hi <strong>{username}</strong>,
</p>
<p style="margin: 0 0 30px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    We received a request to reset your password. Use the code below to reset your password.
</p>
<div style="margin: 30px 0; text-align: center;">
    <div style="display: inline-block; background: linear-gradient(135deg, #ef4444 0%, #f97316 100%); padding: 4px; border-radius: 12px;">
        <div style="background: #fafafa; border-radius: 8px; padding: 20px 40px;">
            <span style="font-size: 32px; font-weight: 700; letter-spacing: 8px; color: #18181b; font-family: 'Courier New', monospace;">{reset_code}</span>
        </div>
    </div>
</div>
<p style="margin: 20px 0 0; color: #71717a; font-size: 14px; line-height: 1.6; text-align: center;">
    Enter this code on the password reset page to create a new password.
</p>
<p style="margin: 30px 0 0; color: #71717a; font-size: 14px; line-height: 1.6;">
    This code will expire in 1 hour. If you didn't request a password reset, you can safely ignore this email.
</p>"#,
        username = username,
        reset_code = reset_code
    );

    let text_content = format!(
        r#"Reset Your Password

Hi {username},

We received a request to reset your password. Use the code below to reset your password.

Your reset code: {reset_code}

Enter this code on the password reset page to create a new password.

This code will expire in 1 hour. If you didn't request a password reset, you can safely ignore this email."#,
        username = username,
        reset_code = reset_code
    );

    EmailTemplate {
        html: wrap_html("Reset Your Password", &html_content),
        text: wrap_text(&text_content),
    }
}
