//! Account and security emails (sign-in code, step-up code, email change, new sign-in
//! alert, welcome, generic notification). Every interpolated value is
//! escaped; codes are shown large and nothing beyond the display name is disclosed.

use super::base::{button, code_block, escape_html, note, paragraph, plain, wrap_html, wrap_text};
use super::EmailTemplate;

fn greeting(name: &str) -> (String, String) {
    (
        paragraph(&format!("Hi {},", escape_html(name))),
        format!("Hi {},", plain(name)),
    )
}

/// Where and when a code was requested, shown so an owner can spot a request that was
/// not theirs. Every field is optional; nothing here is a secret.
#[derive(Debug, Clone, Default)]
pub struct RequestDetails {
    /// Shortened User-Agent of the requesting client.
    pub client: Option<String>,
    /// Partially masked client address (e.g. `102.89.x.x`).
    pub address: Option<String>,
    /// `YYYY-MM-DD HH:MM` (UTC).
    pub when_utc: String,
}

impl RequestDetails {
    fn sentence(&self) -> String {
        let mut s = format!("Requested on {} (UTC)", self.when_utc);
        if let Some(client) = &self.client {
            s.push_str(&format!(" from {client}"));
        }
        if let Some(address) = &self.address {
            s.push_str(&format!(" (network {address})"));
        }
        s.push('.');
        s
    }
}

/// Passwordless sign-in code (valid `ttl`), with an optional one-click link that signs
/// in on this device. The same email serves sign-up: the first sign-in creates the
/// account.
pub fn sign_in_code_email(
    code: &str,
    ttl: &str,
    link: Option<&str>,
    details: &RequestDetails,
) -> EmailTemplate {
    let link_html = link
        .map(|url| {
            format!(
                "{}{}",
                paragraph("Or sign in with one click on this device:"),
                button("Sign in to CF Meet", &escape_html(url))
            )
        })
        .unwrap_or_default();
    let link_text = link
        .map(|url| format!("\n\nOr sign in with one click on this device:\n{}", plain(url)))
        .unwrap_or_default();
    let html = format!(
        "{}{}{}{link_html}{}{}",
        paragraph("Use this code to sign in to CF Meet:"),
        code_block(code),
        paragraph(&format!("The code expires in {}.", escape_html(ttl))),
        paragraph(&escape_html(&details.sentence())),
        note("If you did not try to sign in, you can ignore this email: nobody can sign in without this code. Never share it with anyone; CF Meet staff will never ask for it."),
    );
    let text = format!(
        "Use this code to sign in to CF Meet:\n\n    {code}\n\nThe code expires in {ttl}.{link_text}\n\n{details}\n\nIf you did not try to sign in, you can ignore this email: nobody can sign in without this code. Never share it with anyone.",
        code = plain(code),
        ttl = plain(ttl),
        details = plain(&details.sentence()),
    );
    EmailTemplate {
        html: wrap_html("Your CF Meet sign-in code", &html),
        text: wrap_text(&format!("Your CF Meet sign-in code\n\n{text}")),
    }
}

/// Step-up confirmation code for a sensitive action (`action`, e.g. "deactivate your
/// account").
pub fn step_up_code_email(username: &str, code: &str, ttl: &str, action: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!(
        "{hi_html}{}{}{}{}",
        paragraph(&format!(
            "Use this code to confirm that you want to {}:",
            escape_html(action)
        )),
        code_block(code),
        paragraph(&format!("The code expires in {}.", escape_html(ttl))),
        note("If you did not ask for this, someone may be signed in to your account: open CF Meet and sign out all devices. Never share this code with anyone."),
    );
    let text = format!(
        "{hi_text}\n\nUse this code to confirm that you want to {action}:\n\n    {code}\n\nThe code expires in {ttl}.\n\nIf you did not ask for this, someone may be signed in to your account: open CF Meet and sign out all devices. Never share this code with anyone.",
        action = plain(action),
        code = plain(code),
        ttl = plain(ttl),
    );
    EmailTemplate {
        html: wrap_html("Confirm it's you", &html),
        text: wrap_text(&format!("Confirm it's you\n\n{text}")),
    }
}

/// Code proving ownership of a new address during an email change.
pub fn email_change_code_email(username: &str, code: &str, ttl: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let html = format!(
        "{hi_html}{}{}{}{}",
        paragraph("Use this code to make this the email address of your CF Meet account:"),
        code_block(code),
        paragraph(&format!("The code expires in {}.", escape_html(ttl))),
        note("If you did not ask for this, ignore this email: nothing changes without this code."),
    );
    let text = format!(
        "{hi_text}\n\nUse this code to make this the email address of your CF Meet account:\n\n    {code}\n\nThe code expires in {ttl}.\n\nIf you did not ask for this, ignore this email: nothing changes without this code.",
        code = plain(code),
        ttl = plain(ttl),
    );
    EmailTemplate {
        html: wrap_html("Confirm your new email address", &html),
        text: wrap_text(&format!("Confirm your new email address\n\n{text}")),
    }
}

/// Sent to an address that already belongs to an account when someone tries to move
/// another account to it (no code: the change cannot happen).
pub fn email_in_use_notice_email() -> EmailTemplate {
    let html = format!(
        "{}{}",
        paragraph("Someone tried to change the email address of a CF Meet account to this address. This address already has a CF Meet account, so nothing was changed."),
        note("You don't need to do anything. If you keep receiving these emails, contact CF Meet support."),
    );
    let text = "Someone tried to change the email address of a CF Meet account to this address. This address already has a CF Meet account, so nothing was changed.\n\nYou don't need to do anything. If you keep receiving these emails, contact CF Meet support.";
    EmailTemplate {
        html: wrap_html("Email change attempt", &html),
        text: wrap_text(&format!("Email change attempt\n\n{text}")),
    }
}

/// Sent to the PREVIOUS address after an email change, with a single-use undo link
/// (`undo_url`, valid `window`). `None` (support-assisted change): no undo link.
pub fn email_changed_old_address_email(
    username: &str,
    new_email_masked: &str,
    when_utc: &str,
    undo_url: Option<&str>,
    window: &str,
) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let what = format!(
        "The email address of your CF Meet account was changed to {new_email_masked} on {when_utc} (UTC). Other signed-in devices were signed out."
    );
    let (undo_html, undo_text) = match undo_url {
        Some(url) => (
            format!(
                "{}{}",
                paragraph(&format!(
                    "If this wasn't you, undo the change within {}. This signs out every device and gives the account back to this address:",
                    escape_html(window)
                )),
                button("Undo this change", &escape_html(url))
            ),
            format!(
                "\n\nIf this wasn't you, undo the change within {}. This signs out every device and gives the account back to this address:\n{}",
                plain(window),
                plain(url)
            ),
        ),
        None => (
            note("This change was made by CF Meet support after verifying the account owner. If you did not ask for it, contact CF Meet support immediately."),
            "\n\nThis change was made by CF Meet support after verifying the account owner. If you did not ask for it, contact CF Meet support immediately.".to_string(),
        ),
    };
    let html = format!("{hi_html}{}{undo_html}", paragraph(&escape_html(&what)));
    let text = format!("{hi_text}\n\n{}{undo_text}", plain(&what));
    EmailTemplate {
        html: wrap_html("Your email address was changed", &html),
        text: wrap_text(&format!("Your email address was changed\n\n{text}")),
    }
}

/// Sent to the NEW address once it is the account's address.
pub fn email_changed_new_address_email(username: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let msg = "This is now the email address of your CF Meet account. Use it to sign in from now on.";
    EmailTemplate {
        html: wrap_html(
            "Email address updated",
            &format!("{hi_html}{}", paragraph(msg)),
        ),
        text: wrap_text(&format!("Email address updated\n\n{hi_text}\n\n{msg}")),
    }
}

/// Security alert: a sign-in from a device not seen on this account recently.
pub fn new_sign_in_email(username: &str, details: &RequestDetails, sessions_url: &str) -> EmailTemplate {
    let (hi_html, hi_text) = greeting(username);
    let what = format!(
        "Your CF Meet account was just signed in to from a new device. {}",
        details.sentence().replacen("Requested on", "Signed in on", 1)
    );
    let html = format!(
        "{hi_html}{}{}{}",
        paragraph(&escape_html(&what)),
        paragraph("If this was you, there is nothing to do. If not, sign out every device now:"),
        button("Review signed-in devices", &escape_html(sessions_url)),
    );
    let text = format!(
        "{hi_text}\n\n{}\n\nIf this was you, there is nothing to do. If not, sign out every device now:\n{}",
        plain(&what),
        plain(sessions_url)
    );
    EmailTemplate {
        html: wrap_html("New sign-in to your account", &html),
        text: wrap_text(&format!("New sign-in to your account\n\n{text}")),
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

    fn details() -> RequestDetails {
        RequestDetails {
            client: Some("<script>Chrome".into()),
            address: Some("102.89.x.x".into()),
            when_utc: "2026-11-17 14:02".into(),
        }
    }

    #[test]
    fn sign_in_email_shows_code_link_and_escaped_request_details() {
        let t = sign_in_code_email(
            "012345",
            "10 minutes",
            Some("https://meet.example/auth/magic?email=a%40b.c&token=ab"),
            &details(),
        );
        assert!(t.html.contains("012345") && t.text.contains("012345"));
        assert!(t.text.contains("10 minutes"));
        assert!(t.text.contains("token=ab"));
        assert!(t.html.contains("&lt;script&gt;Chrome"));
        assert!(!t.html.contains("<script>"));
        assert!(t.text.contains("2026-11-17 14:02") && t.text.contains("102.89.x.x"));
        let no_link = sign_in_code_email("012345", "10 minutes", None, &RequestDetails::default());
        assert!(!no_link.text.contains("one click"));
    }

    #[test]
    fn security_emails_render() {
        let s = step_up_code_email("<b>Ada</b>", "654321", "10 minutes", "deactivate your account");
        assert!(s.html.contains("654321") && s.html.contains("&lt;b&gt;Ada"));
        assert!(s.text.contains("deactivate your account"));
        let old = email_changed_old_address_email(
            "Ada",
            "n***@example.com",
            "2026-11-17 14:02",
            Some("https://meet.example/auth/email-change/undo?token=x"),
            "7 days",
        );
        assert!(old.text.contains("n***@example.com") && old.text.contains("undo?token=x"));
        let support = email_changed_old_address_email("Ada", "n***@e.com", "t", None, "7 days");
        assert!(support.text.contains("CF Meet support") && !support.text.contains("Undo"));
        assert!(email_change_code_email("Ada", "111222", "10 minutes").text.contains("111222"));
        assert!(email_in_use_notice_email().text.contains("nothing was changed"));
        assert!(email_changed_new_address_email("Ada").text.contains("sign in"));
        let n = new_sign_in_email("Ada", &details(), "https://meet.example/settings/sessions");
        assert!(n.text.contains("Signed in on 2026-11-17 14:02"));
        assert!(n.html.contains("settings/sessions"));
        let g = notification_email("Ada", "Hi <there>", "a & b");
        assert!(g.html.contains("a &amp; b") && g.html.contains("Hi &lt;there&gt;"));
    }
}
