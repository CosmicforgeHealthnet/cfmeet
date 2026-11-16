//! Account and security emails (verification code, password reset code, welcome,
//! password-changed security notice, generic notification). Every interpolated value is
//! escaped; codes are shown large and nothing beyond the display name is disclosed.

use super::base::{button, code_block, escape_html, note, paragraph, plain, wrap_html, wrap_text};
use super::EmailTemplate;

fn greeting(name: &str) -> (String, String) {
    (
        paragraph(&format!("Hi {},", escape_html(name))),
        format!("Hi {},", plain(name)),
    )
}

/// Registration / re-verification code (valid `ttl` — e.g. "24 hours").
pub fn verification_email(username: &str, code: &str, ttl: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!(
        "{hi_html}{}{}{}{}",
        paragraph("Use this code to verify your email address for CF Meet:"),
        code_block(code),
        paragraph(&format!("The code expires in {}.", escape_html(ttl))),
        note("If you did not create a CF Meet account, you can ignore this email. Never share this code with anyone; CF Meet staff will never ask for it."),
    );
    let text = format!(
        "{hi_text}\n\nUse this code to verify your email address for CF Meet:\n\n    {code}\n\nThe code expires in {ttl}.\n\nIf you did not create a CF Meet account, you can ignore this email. Never share this code with anyone.",
        code = plain(code),
        ttl = plain(ttl),
    );
    EmailTemplate {
        html: wrap_html("Verify your email address", &html),
        text: wrap_text(&format!("Verify your email address\n\n{text}")),
    }
}

/// Password reset code (valid `ttl`).
pub fn password_reset_email(username: &str, code: &str, ttl: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!(
        "{hi_html}{}{}{}{}",
        paragraph("We received a request to reset your CF Meet password. Use this code to choose a new password:"),
        code_block(code),
        paragraph(&format!("The code expires in {}.", escape_html(ttl))),
        note("If you did not request a password reset, ignore this email: your password stays unchanged. Never share this code with anyone."),
    );
    let text = format!(
        "{hi_text}\n\nWe received a request to reset your CF Meet password. Use this code to choose a new password:\n\n    {code}\n\nThe code expires in {ttl}.\n\nIf you did not request a password reset, ignore this email: your password stays unchanged. Never share this code with anyone.",
        code = plain(code),
        ttl = plain(ttl),
    );
    EmailTemplate {
        html: wrap_html("Reset your password", &html),
        text: wrap_text(&format!("Reset your password\n\n{text}")),
    }
}

/// Sent once the address is verified.
pub fn welcome_email(username: &str, app_url: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!(
        "{hi_html}{}{}",
        paragraph("Your email address is verified and your CF Meet account is ready."),
        button("Open CF Meet", &escape_html(app_url)),
    );
    let text = format!(
        "{hi_text}\n\nYour email address is verified and your CF Meet account is ready.\n\nOpen CF Meet: {}",
        plain(app_url)
    );
    EmailTemplate {
        html: wrap_html("Welcome to CF Meet", &html),
        text: wrap_text(&format!("Welcome to CF Meet\n\n{text}")),
    }
}

/// Security notice after a password change or reset (no secrets, no links to act on).
pub fn password_changed_email(username: &str, how: &str, when_utc: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!(
        "{hi_html}{}{}",
        paragraph(&format!(
            "The password of your CF Meet account was {} on {} (UTC). Other signed-in sessions were signed out.",
            escape_html(how),
            escape_html(when_utc)
        )),
        note("If this was not you, reset your password immediately from the CF Meet sign-in page and review your active sessions."),
    );
    let text = format!(
        "{hi_text}\n\nThe password of your CF Meet account was {} on {} (UTC). Other signed-in sessions were signed out.\n\nIf this was not you, reset your password immediately from the CF Meet sign-in page and review your active sessions.",
        plain(how),
        plain(when_utc)
    );
    EmailTemplate {
        html: wrap_html("Your password was changed", &html),
        text: wrap_text(&format!("Your password was changed\n\n{text}")),
    }
}

/// Generic notification with an escaped message.
pub fn notification_email(username: &str, subject: &str, message: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!("{hi_html}{}", paragraph(&escape_html(message)));
    let text = format!("{hi_text}\n\n{}", plain(message));
    EmailTemplate {
        html: wrap_html(&escape_html(subject), &html),
        text: wrap_text(&format!("{}\n\n{text}", plain(subject))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otp_email_shows_code_and_escapes_name() {
        let t = verification_email("<b>Ada</b>", "123456", "24 hours");
        assert!(t.html.contains("123456"));
        assert!(t.text.contains("123456"));
        assert!(!t.html.contains("<b>Ada</b>"));
        assert!(t.html.contains("&lt;b&gt;Ada&lt;/b&gt;"));
        assert!(t.text.contains("24 hours"));
        assert!(!t.html.contains("HealthNet"));
    }

    #[test]
    fn reset_and_notice_render() {
        let r = password_reset_email("Ada", "654321", "1 hour");
        assert!(r.html.contains("654321") && r.text.contains("1 hour"));
        let n = password_changed_email("Ada", "reset", "2026-11-12 10:00");
        assert!(n.text.contains("reset on 2026-11-12 10:00"));
        let g = notification_email("Ada", "Hi <there>", "a & b");
        assert!(g.html.contains("a &amp; b") && g.html.contains("Hi &lt;there&gt;"));
    }
}
