//! CF Meet transactional email layout (unified email delivery).
//!
//! One responsive, accessible layout for every email: CF Meet identity and logo, a clear
//! title, the content, an optional primary action and a consistent footer. Callers pass
//! already-escaped HTML fragments ([`escape_html`]); plain-text bodies are built in
//! parallel by every template (multipart/alternative).

/// Public CF Meet logo (served by the CF Meet website).
pub const LOGO_URL: &str = "https://www.cfmeet.com/CFMeet.png";
/// Brand colours (web app palette).
pub const BRAND: &str = "#006BFF";
pub const INK: &str = "#0F172A";
const MUTED: &str = "#64748B";

/// Minimal HTML escaping for text and attribute values; control characters are dropped.
pub fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// Plain-text variant of a user-provided value: control characters stripped (no header
/// injection, no terminal escape sequences).
pub fn plain(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

/// Wraps already-escaped content in the CF Meet HTML layout. `title` must be escaped.
pub fn wrap_html(title: &str, content: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="color-scheme" content="light">
<title>{title}</title>
</head>
<body style="margin:0;padding:0;background-color:#F1F5F9;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,'Helvetica Neue',Arial,sans-serif;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="border-collapse:collapse;background-color:#F1F5F9;">
  <tr>
    <td align="center" style="padding:32px 12px;">
      <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:560px;border-collapse:collapse;background-color:#FFFFFF;border-radius:12px;">
        <tr>
          <td style="padding:28px 32px 8px;text-align:left;">
            <img src="{logo}" width="40" height="40" alt="CF Meet" style="display:inline-block;vertical-align:middle;border:0;">
            <span style="display:inline-block;vertical-align:middle;margin-left:10px;color:{ink};font-size:20px;font-weight:700;">CF Meet</span>
          </td>
        </tr>
        <tr>
          <td style="padding:16px 32px 32px;color:{ink};">
            <h1 style="margin:0 0 16px;color:{ink};font-size:22px;line-height:1.3;font-weight:700;">{title}</h1>
            {content}
          </td>
        </tr>
        <tr>
          <td style="padding:20px 32px;border-top:1px solid #E2E8F0;color:{muted};font-size:12px;line-height:1.6;">
            You received this email because of activity on your CF Meet account.<br>
            CF Meet by CosmicForge Tech Limited &middot; <a href="https://cfmeet.com" style="color:{muted};">cfmeet.com</a><br>
            This mailbox is not monitored; please do not reply.
          </td>
        </tr>
      </table>
    </td>
  </tr>
</table>
</body>
</html>"#,
        title = title,
        content = content,
        logo = LOGO_URL,
        ink = INK,
        muted = MUTED,
    )
}

/// A paragraph of already-escaped HTML.
pub fn paragraph(html: &str) -> String {
    format!(r#"<p style="margin:0 0 16px;color:#334155;font-size:16px;line-height:1.6;">{html}</p>"#)
}

/// A small print paragraph of already-escaped HTML.
pub fn note(html: &str) -> String {
    format!(r#"<p style="margin:24px 0 0;color:{MUTED};font-size:13px;line-height:1.5;">{html}</p>"#)
}

/// Primary call to action. `text` and `url` must be escaped.
pub fn button(text: &str, url: &str) -> String {
    format!(
        r#"<table role="presentation" cellpadding="0" cellspacing="0" style="border-collapse:collapse;margin:8px 0 16px;">
  <tr>
    <td style="border-radius:8px;background-color:{BRAND};">
      <a href="{url}" style="display:inline-block;padding:14px 28px;color:#FFFFFF;text-decoration:none;font-size:16px;font-weight:600;border-radius:8px;">{text}</a>
    </td>
  </tr>
</table>"#
    )
}

/// A one-time code, large and easy to read (digits spaced; `aria-label` reads it whole).
pub fn code_block(code: &str) -> String {
    let code = escape_html(code);
    format!(
        r#"<div style="margin:24px 0;text-align:center;">
  <div role="text" aria-label="Your code is {code}" style="display:inline-block;padding:16px 28px;border:1px solid #CBD5E1;border-radius:10px;background-color:#F8FAFC;font-family:'SFMono-Regular',Menlo,Consolas,'Courier New',monospace;font-size:32px;font-weight:700;letter-spacing:8px;color:{INK};">{code}</div>
</div>"#
    )
}

/// Wraps plain-text content with the CF Meet footer.
pub fn wrap_text(content: &str) -> String {
    format!(
        "{content}\n\n--\nCF Meet by CosmicForge Tech Limited - https://cfmeet.com\nThis mailbox is not monitored; please do not reply."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_carries_identity_and_escaped_title() {
        let html = wrap_html(&escape_html("<x>"), "<p>body</p>");
        assert!(html.contains("CF Meet"));
        assert!(html.contains(LOGO_URL));
        assert!(html.contains("&lt;x&gt;"));
        assert!(!html.contains("CosmicForge HealthNet"));
    }

    #[test]
    fn code_is_escaped() {
        assert!(code_block("<1>").contains("&lt;1&gt;"));
    }
}
