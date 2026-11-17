//! Template registry (roadmap N3): every routed event type renders to
//! * an inbox title / body from its stored `params` (never codes, tokens, addresses or URLs);
//! * an email subject / HTML / text (codes and tokens come from the opened sealed part and
//!   exist only in the sealed body);
//! * a push payload, which is metadata only (the push transport refuses anything but its
//!   five metadata keys): message wake-ups carry the Messaging metadata, every other
//!   category wakes the client to read its inbox.
//!
//! Account and meeting emails are rendered with the ported Meeting templates
//! ([`email`]), so the subject/body digest equals the Meeting output for the same input.

pub mod email;

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use self::email::meeting::{render as render_notice, NotificationBody};
use self::email::{
    notification_email, password_changed_email, password_reset_email, verification_email,
};

/// Meeting notice footnote (Meeting `notifications.rs` FOOTNOTE, verbatim).
pub const MEETING_FOOTNOTE: &str = "You receive this email because you were invited to or take part in this CF Meet meeting. You can turn meeting notifications off in your CF Meet notification settings.";

/// Verification codes are valid 24 hours, reset codes 1 hour (Meeting `services::email`).
pub fn ttl_words(minutes: i64) -> String {
    match minutes {
        m if m > 0 && m % 1440 == 0 => {
            let d = m / 1440;
            if d == 1 {
                "24 hours".into()
            } else {
                format!("{d} days")
            }
        }
        m if m > 0 && m % 60 == 0 => {
            let h = m / 60;
            if h == 1 {
                "1 hour".into()
            } else {
                format!("{h} hours")
            }
        }
        1 => "1 minute".into(),
        m => format!("{} minutes", m.max(1)),
    }
}

/// Meeting `format_time`, verbatim.
pub fn format_time(at: DateTime<Utc>, preferred: Option<&str>, fallback: &str) -> String {
    let tz = preferred
        .and_then(|z| z.parse::<chrono_tz::Tz>().ok())
        .or_else(|| fallback.parse::<chrono_tz::Tz>().ok())
        .unwrap_or(chrono_tz::UTC);
    format!(
        "{} ({})",
        at.with_timezone(&tz).format("%a %-d %b %Y, %H:%M"),
        tz.name()
    )
}

fn clean(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Conference facts from the Meeting authority (X2), already filtered by Meeting's
/// protected-title rule.
#[derive(Debug, Clone, Default)]
pub struct MeetingFacts {
    pub presented_title: String,
    pub join_url: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub previous_start: Option<DateTime<Utc>>,
    pub space_time_zone: String,
}

/// What a renderer may know about the recipient (from X1 / X2; never stored in params).
#[derive(Debug, Clone, Default)]
pub struct RecipientFacts {
    pub display_name: Option<String>,
    pub time_zone: Option<String>,
}

/// Sealed values opened for this render only.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub code: Option<String>,
    pub token: Option<String>,
}

/// A rendered email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedEmail {
    pub subject: String,
    pub html: String,
    pub text: String,
}

impl RenderedEmail {
    /// SHA-256 over subject, NUL, text: the template-parity digest recorded in shadow
    /// observations (never the content itself).
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.subject.as_bytes());
        h.update([0u8]);
        h.update(self.text.as_bytes());
        hex::encode(h.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("a sealed value required by the template is unavailable")]
    SecretUnavailable,
    #[error("conference facts are unavailable")]
    MeetingUnavailable,
    #[error("the template has no email form")]
    NoEmail,
}

fn data_str<'a>(data: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    data.get(key).and_then(Value::as_str)
}

fn data_i64(data: &Map<String, Value>, key: &str) -> Option<i64> {
    data.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

/// The params stored with a notification / inbox item: the event data minus anything a
/// renderer must not keep (data never carries secrets by contract; this is the second
/// line of defence) and bounded in size.
pub fn params_of(data: &std::collections::BTreeMap<String, Value>) -> Value {
    let mut out = Map::new();
    for (k, v) in data {
        let lower = k.to_ascii_lowercase();
        if cf_notification_client::envelope::FORBIDDEN_DATA_KEY_PARTS
            .iter()
            .any(|f| lower.contains(f))
        {
            continue;
        }
        match v {
            Value::String(s) => {
                out.insert(k.clone(), Value::String(clean(s, 200)));
            }
            Value::Number(_) | Value::Bool(_) => {
                out.insert(k.clone(), v.clone());
            }
            _ => {}
        }
    }
    Value::Object(out)
}

/// Inbox title and body for a stored item (rendered at read time).
pub fn inbox_text(template: &str, params: &Value) -> (String, String) {
    let empty = Map::new();
    let p = params.as_object().unwrap_or(&empty);
    let role = data_str(p, "role").map(|r| r.to_ascii_lowercase().replace('_', " "));
    let (title, body): (&str, String) = match template {
        "org.invitation.created" => (
            "Organization invitation",
            "You were invited to join an organization on CF Meet.".into(),
        ),
        "org.invitation.accepted" => (
            "Invitation accepted",
            "Your organization invitation was accepted.".into(),
        ),
        "org.invitation.declined" => (
            "Invitation declined",
            "Your organization invitation was declined.".into(),
        ),
        "org.invitation.revoked" => (
            "Invitation withdrawn",
            "An organization invitation was withdrawn.".into(),
        ),
        "org.invitation.expired" => (
            "Invitation expired",
            "An organization invitation expired.".into(),
        ),
        "org.membership.role_changed" => (
            "Role changed",
            match role {
                Some(r) => format!("Your organization role is now {r}."),
                None => "Your organization role changed.".into(),
            },
        ),
        "org.membership.suspended" => (
            "Membership suspended",
            "Your organization membership was suspended.".into(),
        ),
        "org.membership.removed" => (
            "Removed from organization",
            "You were removed from an organization.".into(),
        ),
        "org.ownership.transferred" => (
            "Ownership transferred",
            "The ownership of an organization was transferred.".into(),
        ),
        "org.organization.suspended" => (
            "Organization suspended",
            "An organization you belong to was suspended.".into(),
        ),
        "contact.requested" => (
            "New contact request",
            "Someone wants to connect with you.".into(),
        ),
        "contact.connected" => ("Contact connected", "You are now connected.".into()),
        "message.created" => ("New message", "You have a new message.".into()),
        "message.member_invited" => (
            "Added to a group",
            "You were added to a group conversation.".into(),
        ),
        "call.missed" => ("Missed call", "You missed a call.".into()),
        "call.voicemail" => ("New voicemail", "You have a new voice message.".into()),
        "call.recording" => ("Recording ready", "A call recording is ready.".into()),
        "meeting.reminder" => (
            "Meeting reminder",
            "A meeting you take part in starts soon.".into(),
        ),
        "meeting.rescheduled" => (
            "Meeting rescheduled",
            "A meeting you take part in was rescheduled.".into(),
        ),
        "meeting.cancelled" => (
            "Meeting cancelled",
            "A meeting you take part in was cancelled.".into(),
        ),
        "account.password_changed" => (
            "Password changed",
            "Your CF Meet password was changed. If this was not you, reset it now.".into(),
        ),
        "account.device_linked" => (
            "New device linked",
            "A new device was linked to your CF Meet account.".into(),
        ),
        "developer.credential_expiring" => (
            "Credential expiring",
            "A developer credential expires soon.".into(),
        ),
        "developer.credential_expired" => (
            "Credential expired",
            "A developer credential expired.".into(),
        ),
        "developer.production_access" => (
            "Production access decided",
            match data_str(p, "decision") {
                Some("APPROVED") => "Your production access request was approved.".into(),
                Some("REJECTED") => "Your production access request was not approved.".into(),
                _ => "Your production access request was decided.".into(),
            },
        ),
        _ => ("Notification", "You have a new notification.".into()),
    };
    (title.to_string(), body)
}

/// Push payload (metadata only; keys within the push transport's whitelist).
pub fn push_payload(
    template: &str,
    data: &std::collections::BTreeMap<String, Value>,
    inbox_version: i64,
) -> Value {
    if template == "message.created" {
        let mut m = Map::new();
        m.insert("v".into(), json!(1));
        m.insert("type".into(), json!("message"));
        if let Some(c) = data.get("conversationId").and_then(Value::as_str) {
            m.insert("conversationId".into(), json!(c));
        }
        // Parity with Messaging's legacy job payload (the 5 metadata keys `v`, `type`,
        // `conversationId`, `eventSequence`, `sequence`): `sequence` is the message
        // sequence, optional in the event (1.1.0).
        for key in ["eventSequence", "sequence"] {
            if let Some(s) = data.get(key) {
                if s.is_number() || s.is_string() {
                    m.insert(key.into(), s.clone());
                }
            }
        }
        return Value::Object(m);
    }
    json!({ "v": 1, "type": "inbox", "sequence": inbox_version })
}

/// Email rendering for one recipient.
pub fn render_email(
    template: &str,
    data: &std::collections::BTreeMap<String, Value>,
    recipient: &RecipientFacts,
    secrets: &Secrets,
    meeting: Option<&MeetingFacts>,
    app_url: &str,
) -> Result<RenderedEmail, RenderError> {
    let obj: Map<String, Value> = data.clone().into_iter().collect();
    let name = recipient.display_name.as_deref().unwrap_or("there");
    let ttl = data_i64(&obj, "ttlMinutes").map(ttl_words);
    let code = || {
        secrets
            .code
            .as_deref()
            .ok_or(RenderError::SecretUnavailable)
    };
    let rendered = match template {
        "account.verification_code" => {
            let t = verification_email(name, code()?, ttl.as_deref().unwrap_or("24 hours"));
            RenderedEmail {
                subject: "Your CF Meet verification code".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.password_reset_code" => {
            let t = password_reset_email(name, code()?, ttl.as_deref().unwrap_or("1 hour"));
            RenderedEmail {
                subject: "Reset your CF Meet password".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.device_step_up_code" => {
            let subject = "Your CF Meet device verification code";
            let message = format!(
                "Use this code to confirm the new device on your CF Meet account: {}. It expires in {}. If this was not you, change your password.",
                code()?,
                ttl.as_deref().unwrap_or("10 minutes")
            );
            let t = notification_email(name, subject, &message);
            RenderedEmail {
                subject: subject.into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.password_changed" => {
            let when = data_str(&obj, "changedAt")
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&Utc).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            let how = data_str(&obj, "how").unwrap_or("changed");
            let t = password_changed_email(name, how, &when);
            RenderedEmail {
                subject: "Your CF Meet password was changed".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.device_linked" => {
            let subject = "A new device was linked to your CF Meet account";
            let message = "A new device was linked to your CF Meet account. If this was not you, revoke the device in your security settings and change your password.";
            let t = notification_email(name, subject, message);
            RenderedEmail {
                subject: subject.into(),
                html: t.html,
                text: t.text,
            }
        }
        "meeting.reminder" | "meeting.rescheduled" | "meeting.cancelled" => {
            let m = meeting.ok_or(RenderError::MeetingUnavailable)?;
            let tz = recipient.time_zone.as_deref();
            let when = |t: DateTime<Utc>| format_time(t, tz, &m.space_time_zone);
            let title = &m.presented_title;
            let (subject, paragraphs, action) = match template {
                "meeting.reminder" => {
                    let start = m.starts_at.unwrap_or_else(Utc::now);
                    (
                        format!("Reminder: {title} starts at {}", when(start)),
                        vec![format!("\"{title}\" starts at {}.", when(start))],
                        Some(("Join meeting", m.join_url.as_str())),
                    )
                }
                "meeting.rescheduled" => {
                    let to = m.starts_at.unwrap_or_else(Utc::now);
                    let mut p = vec![format!("\"{title}\" has been rescheduled.")];
                    if let Some(from) = m.previous_start {
                        p.push(format!("Previously: {}", when(from)));
                    }
                    p.push(format!("New time: {}", when(to)));
                    if let Some(end) = m.ends_at {
                        p.push(format!("Ends: {}", when(end)));
                    }
                    (
                        format!("Rescheduled: {title}"),
                        p,
                        Some(("Open meeting", m.join_url.as_str())),
                    )
                }
                _ => {
                    let mut p = vec![format!("\"{title}\" has been cancelled.")];
                    if let Some(start) = m.starts_at {
                        p.push(format!("It was scheduled for {}.", when(start)));
                    }
                    (format!("Cancelled: {title}"), p, None)
                }
            };
            let subject = clean(&subject, 200);
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: recipient.display_name.as_deref(),
                paragraphs,
                action,
                footnote: Some(MEETING_FOOTNOTE.into()),
            });
            RenderedEmail {
                subject,
                html: t.html,
                text: t.text,
            }
        }
        "meeting.invitation" => {
            let token = secrets
                .token
                .as_deref()
                .ok_or(RenderError::SecretUnavailable)?;
            let title = meeting
                .map(|m| m.presented_title.as_str())
                .unwrap_or("a CF Meet meeting");
            let link = format!("{}/invitations/{}", app_url.trim_end_matches('/'), token);
            let subject = clean(&format!("Invitation: {title}"), 200);
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: recipient.display_name.as_deref(),
                paragraphs: vec![format!("You are invited to \"{title}\" on CF Meet.")],
                action: Some(("Open invitation", &link)),
                footnote: Some(MEETING_FOOTNOTE.into()),
            });
            RenderedEmail {
                subject,
                html: t.html,
                text: t.text,
            }
        }
        "org.invitation.email" => {
            // Sender and branding are configuration (owner decision pending); the wording
            // stays neutral and the link uses the fixed accept path.
            let token = secrets
                .token
                .as_deref()
                .ok_or(RenderError::SecretUnavailable)?;
            let link = format!(
                "{}/organizations/invitations/{}",
                app_url.trim_end_matches('/'),
                token
            );
            let subject = "You're invited to join an organization on CF Meet".to_string();
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: None,
                paragraphs: vec!["You were invited to join an organization on CF Meet.".into()],
                action: Some(("Review invitation", &link)),
                footnote: Some(
                    "If you did not expect this invitation, you can ignore this email.".into(),
                ),
            });
            RenderedEmail {
                subject,
                html: t.html,
                text: t.text,
            }
        }
        "developer.credential_expiring"
        | "developer.credential_expired"
        | "developer.production_access" => {
            let (title, body) = inbox_text(template, &params_of(data));
            let subject = format!("CF Meet Developer Platform: {title}");
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: recipient.display_name.as_deref(),
                paragraphs: vec![body],
                action: Some((
                    "Open Developer Console",
                    &format!("{}/developers", app_url.trim_end_matches('/')),
                )),
                footnote: None,
            });
            RenderedEmail {
                subject,
                html: t.html,
                text: t.text,
            }
        }
        "operator.alert" => {
            let alert = data_str(&obj, "alertName").unwrap_or("alert");
            let severity = data_str(&obj, "severity").unwrap_or("unknown");
            let status = data_str(&obj, "status").unwrap_or("firing");
            let subject = clean(&format!("[CF Meet {severity}] {alert} {status}"), 200);
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: None,
                paragraphs: vec![format!("Alert {alert} is {status} (severity {severity}).")],
                action: None,
                footnote: Some("Sent by the CF Meet Notification Service operator channel.".into()),
            });
            RenderedEmail {
                subject,
                html: t.html,
                text: t.text,
            }
        }
        _ => return Err(RenderError::NoEmail),
    };
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn data(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn verification_email_has_meeting_parity() {
        let r = render_email(
            "account.verification_code",
            &data(&[("ttlMinutes", json!(1440))]),
            &RecipientFacts {
                display_name: Some("Ada".into()),
                time_zone: None,
            },
            &Secrets {
                code: Some("123456".into()),
                token: None,
            },
            None,
            "https://cfmeet.com",
        )
        .unwrap();
        let legacy = verification_email("Ada", "123456", "24 hours");
        assert_eq!(r.subject, "Your CF Meet verification code");
        assert_eq!(r.text, legacy.text);
        assert_eq!(r.html, legacy.html);
    }

    #[test]
    fn codes_never_render_without_the_sealed_part_and_never_reach_params() {
        let e = render_email(
            "account.password_reset_code",
            &data(&[("ttlMinutes", json!(60))]),
            &RecipientFacts::default(),
            &Secrets::default(),
            None,
            "https://cfmeet.com",
        );
        assert_eq!(e, Err(RenderError::SecretUnavailable));
        let p = params_of(&data(&[
            ("ttlMinutes", json!(60)),
            ("code", json!("x")),
            ("accessToken", json!("y")),
        ]));
        assert_eq!(p, json!({"ttlMinutes": 60}));
    }

    #[test]
    fn reminder_matches_the_meeting_compose_rule() {
        let start = DateTime::parse_from_rfc3339("2026-12-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let m = MeetingFacts {
            presented_title: "Weekly".into(),
            join_url: "https://cfmeet.com/abc-defg-hij".into(),
            starts_at: Some(start),
            space_time_zone: "Europe/Berlin".into(),
            ..Default::default()
        };
        let r = render_email(
            "meeting.reminder",
            &data(&[]),
            &RecipientFacts {
                display_name: Some("Ada".into()),
                time_zone: None,
            },
            &Secrets::default(),
            Some(&m),
            "https://cfmeet.com",
        )
        .unwrap();
        assert_eq!(
            r.subject,
            "Reminder: Weekly starts at Tue 1 Dec 2026, 11:00 (Europe/Berlin)"
        );
        assert!(r
            .text
            .contains("Join meeting: https://cfmeet.com/abc-defg-hij"));
        assert!(r.text.contains(MEETING_FOOTNOTE));
    }

    #[test]
    fn push_payloads_stay_within_the_transport_whitelist() {
        let m = push_payload(
            "message.created",
            &data(&[
                ("conversationId", json!("c")),
                ("eventSequence", json!(4)),
                ("securityProfile", json!("X")),
            ]),
            0,
        );
        assert!(cf_push_transport::adapters::metadata(&m).is_some());
        // N9 parity: exactly Messaging's legacy job payload (5 keys), nothing else.
        let full = push_payload(
            "message.created",
            &data(&[
                ("conversationId", json!("c")),
                ("conversationKind", json!("DIRECT")),
                ("eventSequence", json!(4)),
                ("sequence", json!(3)),
                ("securityProfile", json!("PRIVATE_E2EE")),
            ]),
            9,
        );
        assert_eq!(
            full,
            json!({ "v": 1, "type": "message", "conversationId": "c", "eventSequence": 4, "sequence": 3 })
        );
        assert_eq!(
            cf_push_transport::adapters::metadata(&full).unwrap().len(),
            5
        );
        let i = push_payload("contact.requested", &data(&[("actorRef", json!("x"))]), 7);
        assert!(cf_push_transport::adapters::metadata(&i).is_some());
        assert_eq!(i["type"], "inbox");
    }

    #[test]
    fn every_template_has_inbox_text() {
        for r in crate::catalogue::ROUTES {
            let (t, b) = inbox_text(r.template, &json!({}));
            assert!(!t.is_empty() && !b.is_empty());
            if r.has(crate::catalogue::Channel::InApp) {
                assert_ne!(t, "Notification", "{}", r.template);
            }
        }
    }

    #[test]
    fn ttl_words_match_the_legacy_strings() {
        assert_eq!(ttl_words(1440), "24 hours");
        assert_eq!(ttl_words(60), "1 hour");
        assert_eq!(ttl_words(10), "10 minutes");
    }
}
