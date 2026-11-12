//! Blueprint Section 11 notification emails. Every interpolated value is HTML-escaped
//! (titles and names are user-provided); links are built by the service, never taken
//! from a request.

use super::base::{button, wrap_html, wrap_text};
pub use super::base::escape_html;
use super::base::plain;
use super::EmailTemplate;

/// One notification email: a greeting, paragraphs, an optional call to action.
pub struct NotificationBody<'a> {
    pub subject: &'a str,
    pub greeting_name: Option<&'a str>,
    pub paragraphs: Vec<String>,
    pub action: Option<(&'a str, &'a str)>,
    pub footnote: Option<String>,
}

pub fn render(body: &NotificationBody<'_>) -> EmailTemplate {
    let p = |t: &str| {
        format!(
            r#"<p style="margin: 0 0 16px; color: #3f3f46; font-size: 16px; line-height: 1.6;">{}</p>"#,
            escape_html(t)
        )
    };
    let mut html = String::new();
    let mut text = String::new();
    match body.greeting_name {
        Some(name) => {
            html.push_str(&p(&format!("Hi {name},")));
            text.push_str(&format!("Hi {},\n\n", plain(name)));
        }
        None => {
            html.push_str(&p("Hello,"));
            text.push_str("Hello,\n\n");
        }
    }
    for para in &body.paragraphs {
        html.push_str(&p(para));
        text.push_str(&plain(para));
        text.push_str("\n\n");
    }
    if let Some((label, url)) = body.action {
        html.push_str(&button(&escape_html(label), &escape_html(url)));
        text.push_str(&format!("{}: {}\n\n", plain(label), plain(url)));
    }
    if let Some(note) = &body.footnote {
        html.push_str(&format!(
            r#"<p style="margin: 24px 0 0; color: #71717a; font-size: 13px; line-height: 1.5;">{}</p>"#,
            escape_html(note)
        ));
        text.push_str(&plain(note));
        text.push('\n');
    }
    EmailTemplate {
        html: wrap_html(&escape_html(body.subject), &html),
        text: wrap_text(text.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_content_is_escaped() {
        let t = render(&NotificationBody {
            subject: "Reminder: <b>x</b>",
            greeting_name: Some("<script>alert(1)</script>"),
            paragraphs: vec!["Title \"Q3\" & <img src=x>".into()],
            action: Some(("Join", "https://cfmeet.com/join/abc?x=1&y=\"2\"")),
            footnote: Some("Sent by CF Meet".into()),
        });
        assert!(!t.html.contains("<script>"));
        assert!(!t.html.contains("<img"));
        assert!(t.html.contains("&lt;script&gt;"));
        assert!(t.html.contains("&amp;y=&quot;2&quot;"));
        assert!(t.text.contains("Title \"Q3\" & <img src=x>"));
        assert!(t
            .text
            .contains("Join: https://cfmeet.com/join/abc?x=1&y=\"2\""));
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(escape_html("a\u{0007}b\r"), "ab");
        assert_eq!(plain("a\u{001b}[31mb"), "a[31mb");
    }
}
