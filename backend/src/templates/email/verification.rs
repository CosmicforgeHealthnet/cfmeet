use super::base::{wrap_html, wrap_text};
use super::EmailTemplate;

pub fn verification_email(username: &str, verification_code: &str) -> EmailTemplate {
    let html_content = format!(
        r#"<h2 style="margin: 0 0 20px; color: #18181b; font-size: 24px; font-weight: 600;">Verify Your Email Address</h2>
<p style="margin: 0 0 20px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    Hi <strong>{username}</strong>,
</p>
<p style="margin: 0 0 30px; color: #3f3f46; font-size: 16px; line-height: 1.6;">
    Thank you for signing up for CosmicForge HealthNet! Use the verification code below to verify your email address and activate your account.
</p>
<div style="margin: 30px 0; text-align: center;">
    <div style="display: inline-block; background: linear-gradient(135deg, #6366f1 0%, #8b5cf6 100%); padding: 4px; border-radius: 12px;">
        <div style="background: #fafafa; border-radius: 8px; padding: 20px 40px;">
            <span style="font-size: 32px; font-weight: 700; letter-spacing: 8px; color: #18181b; font-family: 'Courier New', monospace;">{verification_code}</span>
        </div>
    </div>
</div>
<p style="margin: 20px 0 0; color: #71717a; font-size: 14px; line-height: 1.6; text-align: center;">
    Enter this code on the verification page to complete your registration.
</p>
<p style="margin: 30px 0 0; color: #71717a; font-size: 14px; line-height: 1.6;">
    This code will expire in 24 hours. If you didn't create an account, you can safely ignore this email.
</p>"#,
        username = username,
        verification_code = verification_code
    );

    let text_content = format!(
        r#"Verify Your Email Address

Hi {username},

Thank you for signing up for CosmicForge HealthNet! Use the verification code below to verify your email address and activate your account.

Your verification code: {verification_code}

Enter this code on the verification page to complete your registration.

This code will expire in 24 hours. If you didn't create an account, you can safely ignore this email."#,
        username = username,
        verification_code = verification_code
    );

    EmailTemplate {
        html: wrap_html("Verify Your Email", &html_content),
        text: wrap_text(&text_content),
    }
}
