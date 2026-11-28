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
    email_change_code_email, email_changed_new_address_email, email_changed_old_address_email,
    email_in_use_notice_email, new_sign_in_email, notification_email, password_changed_email,
    password_reset_email, sign_in_code_email, step_up_code_email, verification_email,
    welcome_email, RequestDetails,
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

/// Sealed values opened for this render only (`Debug` is redacted).
#[derive(Clone, Default)]
pub struct Secrets {
    pub code: Option<String>,
    /// The magic-link, invitation or undo token.
    pub token: Option<String>,
    /// The sealed address (the sign-in magic link carries it, as Meeting's does).
    pub address: Option<String>,
    /// The masked new address of an email-changed notice to the previous address.
    pub masked_address: Option<String>,
    /// The client label and masked network a new sign-in alert shows.
    pub request_client: Option<String>,
    pub request_network: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let r = |v: &Option<String>| v.as_ref().map(|_| "[REDACTED]");
        f.debug_struct("Secrets")
            .field("code", &r(&self.code))
            .field("token", &r(&self.token))
            .field("address", &r(&self.address))
            .field("masked_address", &r(&self.masked_address))
            .field("request_client", &r(&self.request_client))
            .field("request_network", &r(&self.request_network))
            .finish()
    }
}

/// The sentence a step-up code confirms, per `purpose` (Meeting `StepUpPurpose::action`,
/// verbatim). `None`: not a catalogue purpose (never rendered).
pub fn step_up_action(purpose: &str) -> Option<&'static str> {
    match purpose {
        "DEACTIVATE_ACCOUNT" => Some("deactivate your CF Meet account"),
        "CHANGE_EMAIL" => Some("change the email address of your CF Meet account"),
        _ => None,
    }
}

/// Percent-encodes a query value (RFC 3986 unreserved characters pass through; Meeting
/// `services::email::url_encode`, verbatim).
pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The sign-in magic link (Meeting `EmailService::send_sign_in_code`): the web app's
/// `/auth/magic` page POSTs it to `/api/v1/auth/otp/verify-link`.
pub fn magic_link(app_url: &str, address: &str, token: &str) -> String {
    format!(
        "{}/auth/magic?email={}&token={token}",
        app_url.trim_end_matches('/'),
        url_encode(address)
    )
}

/// The undo window as Meeting's email-changed notice words it (Meeting
/// `services::email::EMAIL_CHANGE_UNDO_WINDOW_TEXT`).
pub const EMAIL_CHANGE_UNDO_WINDOW_TEXT: &str = "7 days";

/// The single-use undo link of a self-service email change, rebuilt from the sealed token
/// (Meeting `services::email::undo_link`, verbatim).
pub fn undo_link(app_url: &str, token: &str) -> String {
    format!(
        "{}/auth/email-change/undo?token={token}",
        app_url.trim_end_matches('/')
    )
}

/// The single-use claim link of a developer workspace owner invitation (ADR 0068 A4),
/// rebuilt from the sealed token; the token travels in the fragment only.
pub fn workspace_claim_link(app_url: &str, token: &str) -> String {
    format!(
        "{}/developers/invitations/accept#token={}",
        app_url.trim_end_matches('/'),
        url_encode(token)
    )
}

/// The single-use download link of an organization export (ADR 0068 A4), rebuilt from
/// the sealed token; everything travels in the fragment only, so the token never reaches a
/// server log.
pub fn export_download_link(
    app_url: &str,
    organization: &str,
    export: &str,
    token: &str,
) -> String {
    format!(
        "{}/organizations/exports/download#org={}&export={}&token={}",
        app_url.trim_end_matches('/'),
        url_encode(organization),
        url_encode(export),
        url_encode(token)
    )
}

/// The signed-in devices page of security alerts (Meeting `services::email::sessions_link`,
/// verbatim).
pub fn sessions_link(app_url: &str) -> String {
    format!("{}/settings/sessions", app_url.trim_end_matches('/'))
}

/// `YYYY-MM-DD HH:MM` (UTC) of an RFC 3339 data value, as the legacy emails show times.
fn minute_utc(data: &Map<String, Value>, key: &str) -> Option<String> {
    data_str(data, key)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc).format("%Y-%m-%d %H:%M").to_string())
}

/// The request facts of a sign-in code (`requestedAt`, `requestClient`, `requestNetwork`)
/// as Meeting's `RequestDetails`.
fn request_details(data: &Map<String, Value>) -> RequestDetails {
    RequestDetails {
        client: data_str(data, "requestClient").map(|c| clean(c, 80)),
        address: data_str(data, "requestNetwork").map(|a| clean(a, 64)),
        when_utc: data_str(data, "requestedAt")
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default(),
    }
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
        "org.organization.archived" => (
            "Organization archived",
            "An organization you belong to was archived.".into(),
        ),
        "org.organization.reactivated" => (
            "Organization reactivated",
            "An organization you belong to was reactivated.".into(),
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
        "status.mentioned" => (
            "Mentioned in a status",
            "Someone mentioned you in their status.".into(),
        ),
        "status.shared" => ("Status shared", "Someone shared your status.".into()),
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
        "org.export_ready" => (
            "Organization export ready",
            "An export of your organization is ready to download.".into(),
        ),
        "developer.workspace_owner_invitation" => (
            "Developer workspace invitation",
            "You were invited to own a CF Meet developer workspace.".into(),
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
        "account.sign_in_code" => {
            // No greeting: the address may have no account yet (Meeting PR #97 wording).
            let (Some(token), Some(address)) =
                (secrets.token.as_deref(), secrets.address.as_deref())
            else {
                return Err(RenderError::SecretUnavailable);
            };
            let link = magic_link(app_url, address, token);
            let t = sign_in_code_email(
                code()?,
                ttl.as_deref().unwrap_or("10 minutes"),
                Some(&link),
                &request_details(&obj),
            );
            RenderedEmail {
                subject: "Your CF Meet sign-in code".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.email_change_code" => {
            let t = email_change_code_email(name, code()?, ttl.as_deref().unwrap_or("10 minutes"));
            RenderedEmail {
                subject: "Confirm your new CF Meet email address".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.step_up_code" => {
            let action = data_str(&obj, "purpose")
                .and_then(step_up_action)
                .ok_or(RenderError::NoEmail)?;
            let t = step_up_code_email(
                name,
                code()?,
                ttl.as_deref().unwrap_or("10 minutes"),
                action,
            );
            RenderedEmail {
                subject: "Confirm it's you".into(),
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
        // Account notices (owner decision A, the N12 follow-up): Meeting's templates, the
        // greeting named by the subject account (X1), every other value from the sealed part.
        "account.welcome" => {
            let t = welcome_email(name, app_url);
            RenderedEmail {
                subject: "Welcome to CF Meet".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.new_sign_in" => {
            let details = RequestDetails {
                client: secrets.request_client.clone(),
                address: secrets.request_network.clone(),
                when_utc: minute_utc(&obj, "requestedAt").ok_or(RenderError::NoEmail)?,
            };
            let t = new_sign_in_email(name, &details, &sessions_link(app_url));
            RenderedEmail {
                subject: "New sign-in to your CF Meet account".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.email_in_use" => {
            let t = email_in_use_notice_email();
            RenderedEmail {
                subject: "Email change attempt on CF Meet".into(),
                html: t.html,
                text: t.text,
            }
        }
        "account.email_changed" => match data_str(&obj, "side") {
            Some("OLD") => {
                let masked = secrets
                    .masked_address
                    .as_deref()
                    .ok_or(RenderError::SecretUnavailable)?;
                let when = minute_utc(&obj, "changedAt").ok_or(RenderError::NoEmail)?;
                // No token: the support-assisted change (a support note, no undo link).
                let undo = secrets.token.as_deref().map(|t| undo_link(app_url, t));
                let t = email_changed_old_address_email(
                    name,
                    masked,
                    &when,
                    undo.as_deref(),
                    EMAIL_CHANGE_UNDO_WINDOW_TEXT,
                );
                RenderedEmail {
                    subject: "Your CF Meet email address was changed".into(),
                    html: t.html,
                    text: t.text,
                }
            }
            Some("NEW") if secrets.masked_address.is_none() && secrets.token.is_none() => {
                let t = email_changed_new_address_email(name);
                RenderedEmail {
                    subject: "Your CF Meet email address was updated".into(),
                    html: t.html,
                    text: t.text,
                }
            }
            _ => return Err(RenderError::NoEmail),
        },
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
        "org.export_ready" => {
            // ADR 0068 A4: the owner's single-use download link; ids and the token in the
            // fragment only. Accepting it needs the owner's CF Meet sign-in.
            let token = secrets
                .token
                .as_deref()
                .ok_or(RenderError::SecretUnavailable)?;
            let (Some(org), Some(export)) =
                (data_str(&obj, "organizationId"), data_str(&obj, "exportId"))
            else {
                return Err(RenderError::SecretUnavailable);
            };
            let link = export_download_link(app_url, org, export, token);
            let subject = "Your CF Meet organization export is ready".to_string();
            let mut paragraphs = vec![
                "An export of your organization's data was prepared for you.".into(),
                "Sign in to CF Meet as the organization owner to download it. The link works once."
                    .into(),
            ];
            if let Some(at) = obj
                .get("expiresAt")
                .and_then(Value::as_str)
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            {
                paragraphs.push(format!(
                    "It expires on {}.",
                    format_time(
                        at.with_timezone(&Utc),
                        recipient.time_zone.as_deref(),
                        "UTC"
                    )
                ));
            }
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: recipient.display_name.as_deref(),
                paragraphs,
                action: Some(("Download export", &link)),
                footnote: Some(
                    "If you did not expect this export, contact CF Meet support.".into(),
                ),
            });
            RenderedEmail {
                subject,
                html: t.html,
                text: t.text,
            }
        }
        "developer.workspace_owner_invitation" => {
            // ADR 0068 A4: a single-use claim link to the invited address only. The token is
            // in the fragment, so it never reaches a server log; accepting needs a CF Meet
            // sign-in with this verified address.
            let token = secrets
                .token
                .as_deref()
                .ok_or(RenderError::SecretUnavailable)?;
            let link = workspace_claim_link(app_url, token);
            let subject = "You're invited to own a CF Meet developer workspace".to_string();
            let mut paragraphs = vec![
                "CF Meet registered a developer workspace and invited you as its owner.".into(),
                "Sign in to CF Meet with this email address to accept. The link works once.".into(),
            ];
            if let Some(at) = obj
                .get("expiresAt")
                .and_then(Value::as_str)
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
            {
                paragraphs.push(format!(
                    "It expires on {}.",
                    format_time(
                        at.with_timezone(&Utc),
                        recipient.time_zone.as_deref(),
                        "UTC"
                    )
                ));
            }
            let t = render_notice(&NotificationBody {
                subject: &subject,
                greeting_name: None,
                paragraphs,
                action: Some(("Accept invitation", &link)),
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
                ..Default::default()
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

    /// ADR 0068 A4: the organization export notice renders only with its sealed token, as a
    /// single-use download link with every id and the token in the fragment.
    #[test]
    fn organization_export_needs_its_token_and_keeps_it_in_the_fragment() {
        let d = data(&[
            (
                "organizationId",
                json!("6f1d2c3b-4a59-4e6f-8a7b-9c0d1e2f3a4b"),
            ),
            ("exportId", json!("7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d")),
            ("deliveryId", json!("8b2c3d4e-5f6a-4b7c-9d8e-0f1a2b3c4d5e")),
            ("expiresAt", json!("2026-11-28T12:00:00Z")),
        ]);
        let template = "org.export_ready";
        assert_eq!(
            render_email(
                template,
                &d,
                &RecipientFacts::default(),
                &Secrets::default(),
                None,
                "https://cfmeet.com"
            ),
            Err(RenderError::SecretUnavailable)
        );
        let secrets = Secrets {
            token: Some("exp_fixture_0123456789abcdefghij".into()),
            ..Default::default()
        };
        let e = render_email(
            template,
            &d,
            &RecipientFacts::default(),
            &secrets,
            None,
            "https://cfmeet.com/",
        )
        .unwrap();
        let link = "https://cfmeet.com/organizations/exports/download#org=6f1d2c3b-4a59-4e6f-8a7b-9c0d1e2f3a4b&export=7a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d&token=exp_fixture_0123456789abcdefghij";
        assert!(e.text.contains(link), "{}", e.text);
        // The HTML part escapes the fragment separators.
        assert!(e.html.contains(&link.replace('&', "&amp;")), "{}", e.html);
        assert!(
            !e.text.contains("?token="),
            "the token is never in a query string"
        );
        assert!(e.text.contains("Sat 28 Nov 2026, 12:00 (UTC)"));
        let p = params_of(&d);
        assert!(!p.to_string().contains("exp_0123"));
        let (title, _) = inbox_text(template, &p);
        assert_ne!(title, "Notification");
        let r = crate::catalogue::route("organizations.organization.export_ready").unwrap();
        assert_eq!(r.route_category, "ORGANIZATIONS");
        assert_eq!(r.channels, &[crate::catalogue::Channel::Email]);
        assert_eq!(r.template, template);
    }

    /// ADR 0068 A4: the workspace owner invitation renders only with its sealed token, as a
    /// single-use claim link whose token is in the fragment, never in params.
    #[test]
    fn workspace_owner_invitation_needs_its_token_and_keeps_it_in_the_fragment() {
        let d = data(&[
            (
                "invitationId",
                json!("5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607"),
            ),
            (
                "organizationId",
                json!("6f1d2c3b-4a59-4e6f-8a7b-9c0d1e2f3a4b"),
            ),
            ("expiresAt", json!("2026-11-28T12:00:00Z")),
        ]);
        let template = "developer.workspace_owner_invitation";
        assert_eq!(
            render_email(
                template,
                &d,
                &RecipientFacts::default(),
                &Secrets::default(),
                None,
                "https://cfmeet.com"
            ),
            Err(RenderError::SecretUnavailable)
        );
        let secrets = Secrets {
            token: Some("cfinv_fixture_0123456789abcdefghij".into()),
            address: Some("owner@example.com".into()),
            ..Default::default()
        };
        let e = render_email(
            template,
            &d,
            &RecipientFacts::default(),
            &secrets,
            None,
            "https://cfmeet.com/",
        )
        .unwrap();
        let link =
            "https://cfmeet.com/developers/invitations/accept#token=cfinv_fixture_0123456789abcdefghij";
        assert!(e.text.contains(link) && e.html.contains(link));
        assert!(e.text.contains("Sat 28 Nov 2026, 12:00 (UTC)"));
        assert!(!e.text.contains("owner@example.com"));
        let p = params_of(&d);
        assert!(!p.to_string().contains("cfinv_"));
        let (title, _) = inbox_text(template, &p);
        assert_ne!(title, "Notification");
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

    /// SHA-256 over subject, NUL, text, NUL, html, NUL of the passwordless sign-in email for
    /// the fixed input below. Meeting's `services::email` test
    /// (`the_sign_in_email_matches_the_notification_service_rendering`) computes the same
    /// value from PR #97's legacy template and link, so the two renderers cannot drift
    /// apart unnoticed.
    pub const SIGN_IN_PARITY_DIGEST: &str =
        "611a75c830404a0965fb9a6feaa363b5b70ce86783b3b9ebc4882cf557919d53";

    fn parity_digest(r: &RenderedEmail) -> String {
        let mut h = Sha256::new();
        for part in [&r.subject, &r.text, &r.html] {
            h.update(part.as_bytes());
            h.update([0u8]);
        }
        hex::encode(h.finalize())
    }

    fn sign_in_secrets() -> Secrets {
        Secrets {
            code: Some("482913".into()),
            token: Some("tok_0123456789abcdefghij".into()),
            address: Some("Ada+cf@Example.com".into()),
            ..Default::default()
        }
    }

    fn sign_in_data() -> BTreeMap<String, Value> {
        data(&[
            ("ttlMinutes", json!(10)),
            ("requestedAt", json!("2026-11-17T12:00:00Z")),
            ("requestClient", json!("TestBrowser/1.0")),
            ("requestNetwork", json!("102.89.x.x")),
        ])
    }

    #[test]
    fn sign_in_email_has_meeting_parity() {
        let r = render_email(
            "account.sign_in_code",
            &sign_in_data(),
            &RecipientFacts::default(),
            &sign_in_secrets(),
            None,
            "https://app.cfmeet.com/",
        )
        .unwrap();
        let link = "https://app.cfmeet.com/auth/magic?email=Ada%2Bcf%40Example.com&token=tok_0123456789abcdefghij";
        let legacy = sign_in_code_email(
            "482913",
            "10 minutes",
            Some(link),
            &RequestDetails {
                client: Some("TestBrowser/1.0".into()),
                address: Some("102.89.x.x".into()),
                when_utc: "2026-11-17 12:00".into(),
            },
        );
        assert_eq!(r.subject, "Your CF Meet sign-in code");
        assert_eq!(r.text, legacy.text);
        assert_eq!(r.html, legacy.html);
        assert!(r.text.contains(link));
        assert!(r.text.contains(
            "Requested on 2026-11-17 12:00 (UTC) from TestBrowser/1.0 (network 102.89.x.x)."
        ));
        assert!(
            !r.text.contains("Hi "),
            "no greeting: the address may have no account"
        );
        assert_eq!(parity_digest(&r), SIGN_IN_PARITY_DIGEST);
        // Without the optional request facts the sentence is shorter, as in Meeting.
        let mut bare = sign_in_data();
        bare.remove("requestClient");
        bare.remove("requestNetwork");
        let r = render_email(
            "account.sign_in_code",
            &bare,
            &RecipientFacts::default(),
            &sign_in_secrets(),
            None,
            "https://app.cfmeet.com",
        )
        .unwrap();
        assert!(r.text.contains("Requested on 2026-11-17 12:00 (UTC)."));
    }

    #[test]
    fn sign_in_needs_every_sealed_part_and_codes_need_theirs() {
        for missing in ["code", "token", "address"] {
            let mut s = sign_in_secrets();
            match missing {
                "code" => s.code = None,
                "token" => s.token = None,
                _ => s.address = None,
            }
            assert_eq!(
                render_email(
                    "account.sign_in_code",
                    &sign_in_data(),
                    &RecipientFacts::default(),
                    &s,
                    None,
                    "https://cfmeet.com",
                ),
                Err(RenderError::SecretUnavailable),
                "{missing}"
            );
        }
        for template in ["account.email_change_code", "account.step_up_code"] {
            assert_eq!(
                render_email(
                    template,
                    &data(&[
                        ("ttlMinutes", json!(10)),
                        ("purpose", json!("CHANGE_EMAIL"))
                    ]),
                    &RecipientFacts::default(),
                    &Secrets::default(),
                    None,
                    "https://cfmeet.com",
                ),
                Err(RenderError::SecretUnavailable),
                "{template}"
            );
        }
        let secrets = sign_in_secrets();
        assert!(!format!("{secrets:?}").contains("482913"));
        assert!(!format!("{secrets:?}").contains("Example.com"));
    }

    #[test]
    fn email_change_and_step_up_codes_match_the_meeting_templates() {
        let ada = RecipientFacts {
            display_name: Some("ada".into()),
            time_zone: None,
        };
        let code = Secrets {
            code: Some("715302".into()),
            ..Default::default()
        };
        let r = render_email(
            "account.email_change_code",
            &data(&[("ttlMinutes", json!(10))]),
            &ada,
            &code,
            None,
            "https://cfmeet.com",
        )
        .unwrap();
        let legacy = email_change_code_email("ada", "715302", "10 minutes");
        assert_eq!(r.subject, "Confirm your new CF Meet email address");
        assert_eq!((r.text, r.html), (legacy.text, legacy.html));
        for (purpose, action) in [
            ("DEACTIVATE_ACCOUNT", "deactivate your CF Meet account"),
            (
                "CHANGE_EMAIL",
                "change the email address of your CF Meet account",
            ),
        ] {
            let r = render_email(
                "account.step_up_code",
                &data(&[("ttlMinutes", json!(10)), ("purpose", json!(purpose))]),
                &ada,
                &code,
                None,
                "https://cfmeet.com",
            )
            .unwrap();
            let legacy = step_up_code_email("ada", "715302", "10 minutes", action);
            assert_eq!(r.subject, "Confirm it's you");
            assert_eq!((r.text, r.html), (legacy.text, legacy.html));
        }
        // A purpose outside the closed set is never rendered.
        assert_eq!(
            render_email(
                "account.step_up_code",
                &data(&[("ttlMinutes", json!(10)), ("purpose", json!("OTHER"))]),
                &ada,
                &code,
                None,
                "https://cfmeet.com",
            ),
            Err(RenderError::NoEmail)
        );
        assert_eq!(url_encode("a+b@x.com"), "a%2Bb%40x.com");
    }

    /// SHA-256 over subject, NUL, text, NUL, html, NUL of each account notice for the
    /// fixed inputs below, in this order: welcome, new sign-in, email in use, email changed
    /// (previous address, self-service with the undo link), email changed (previous
    /// address, support-assisted), email changed (new address). Meeting's
    /// `services::email` test (`the_account_notices_match_the_notification_service_
    /// rendering`) computes the same value from its legacy templates and links.
    pub const ACCOUNT_NOTICES_PARITY_DIGEST: &str =
        "4f7c7439dacb3c0316d1809bb34b54c50949d0f95662bf6f290bacb214a8d28d";

    fn ada() -> RecipientFacts {
        RecipientFacts {
            display_name: Some("ada".into()),
            time_zone: None,
        }
    }

    fn notice_secrets() -> Secrets {
        Secrets {
            address: Some("ada@example.com".into()),
            ..Default::default()
        }
    }

    #[test]
    fn account_notices_have_meeting_parity() {
        let app_url = "https://app.cfmeet.com/";
        let changed = |side: &str| {
            data(&[
                ("changedAt", json!("2026-11-17T12:00:00Z")),
                ("side", json!(side)),
            ])
        };
        let old = |token: Option<&str>| Secrets {
            masked_address: Some("n***@example.com".into()),
            token: token.map(str::to_string),
            ..notice_secrets()
        };
        let cases = [
            ("account.welcome", data(&[]), notice_secrets()),
            (
                "account.new_sign_in",
                data(&[("requestedAt", json!("2026-11-17T12:00:00Z"))]),
                Secrets {
                    request_client: Some("TestBrowser/1.0".into()),
                    request_network: Some("102.89.x.x".into()),
                    ..notice_secrets()
                },
            ),
            ("account.email_in_use", data(&[]), notice_secrets()),
            (
                "account.email_changed",
                changed("OLD"),
                old(Some("tok_0123456789abcdefghij")),
            ),
            ("account.email_changed", changed("OLD"), old(None)),
            ("account.email_changed", changed("NEW"), notice_secrets()),
        ];
        let mut h = Sha256::new();
        let mut rendered = Vec::new();
        for (template, d, secrets) in &cases {
            let r = render_email(template, d, &ada(), secrets, None, app_url).unwrap();
            for part in [&r.subject, &r.text, &r.html] {
                h.update(part.as_bytes());
                h.update([0u8]);
            }
            rendered.push(r);
        }
        assert_eq!(hex::encode(h.finalize()), ACCOUNT_NOTICES_PARITY_DIGEST);
        // The links are rebuilt exactly as Meeting builds them.
        assert!(rendered[1]
            .text
            .contains("https://app.cfmeet.com/settings/sessions"));
        assert!(rendered[1].text.contains(
            "Signed in on 2026-11-17 12:00 (UTC) from TestBrowser/1.0 (network 102.89.x.x)."
        ));
        // The fixture value is built, not written after `token=`: the repository secret scan
        // reads a long literal after a credential-like name as a credential.
        let undo_fixture = "tok_0123456789abcdefghij";
        assert!(rendered[3].text.contains(&format!(
            "https://app.cfmeet.com/auth/email-change/undo?token={undo_fixture}"
        )));
        assert!(rendered[3]
            .text
            .contains("changed to n***@example.com on 2026-11-17 12:00 (UTC)"));
        assert!(rendered[3].text.contains("within 7 days"));
        assert!(!rendered[4].text.contains("undo"), "support: no undo link");
        assert!(rendered[4].text.contains("CF Meet support"));
        // Each matches the ported Meeting template directly.
        let legacy = email_changed_old_address_email(
            "ada",
            "n***@example.com",
            "2026-11-17 12:00",
            Some(&undo_link(app_url, "tok_0123456789abcdefghij")),
            "7 days",
        );
        assert_eq!(
            (&rendered[3].text, &rendered[3].html),
            (&legacy.text, &legacy.html)
        );
        let legacy = welcome_email("ada", app_url);
        assert_eq!(rendered[0].text, legacy.text);
        // Without the optional request facts the sentence is shorter, as in Meeting.
        let r = render_email(
            "account.new_sign_in",
            &data(&[("requestedAt", json!("2026-11-17T12:00:00Z"))]),
            &ada(),
            &notice_secrets(),
            None,
            app_url,
        )
        .unwrap();
        assert!(r.text.contains("Signed in on 2026-11-17 12:00 (UTC)."));
    }

    #[test]
    fn account_notices_need_their_sealed_parts_and_a_closed_side() {
        let app_url = "https://cfmeet.com";
        let changed = |side: &str| {
            data(&[
                ("changedAt", json!("2026-11-17T12:00:00Z")),
                ("side", json!(side)),
            ])
        };
        // OLD without the masked address (an unopened sealed part): never rendered.
        assert_eq!(
            render_email(
                "account.email_changed",
                &changed("OLD"),
                &ada(),
                &Secrets::default(),
                None,
                app_url
            ),
            Err(RenderError::SecretUnavailable)
        );
        // NEW never carries the masked address or a token; no other side exists.
        let odd = Secrets {
            token: Some("tok_0123456789abcdefghij".into()),
            ..notice_secrets()
        };
        for (side, secrets) in [("NEW", odd), ("BOTH", notice_secrets())] {
            assert_eq!(
                render_email(
                    "account.email_changed",
                    &changed(side),
                    &ada(),
                    &secrets,
                    None,
                    app_url
                ),
                Err(RenderError::NoEmail),
                "{side}"
            );
        }
        assert_eq!(
            render_email(
                "account.new_sign_in",
                &data(&[]),
                &ada(),
                &notice_secrets(),
                None,
                app_url
            ),
            Err(RenderError::NoEmail)
        );
        // Nothing sealed reaches params or Debug.
        let p = params_of(&data(&[
            ("changedAt", json!("2026-11-17T12:00:00Z")),
            ("side", json!("OLD")),
            ("undoToken", json!("tok_0123456789abcdefghij")),
        ]));
        assert_eq!(
            p,
            json!({"changedAt": "2026-11-17T12:00:00Z", "side": "OLD"})
        );
        let s = Secrets {
            masked_address: Some("n***@example.com".into()),
            request_client: Some("TestBrowser/1.0".into()),
            request_network: Some("102.89.x.x".into()),
            token: Some("tok_0123456789abcdefghij".into()),
            ..notice_secrets()
        };
        let shown = format!("{s:?}");
        for secret in ["n***", "TestBrowser", "102.89", "tok_", "ada@"] {
            assert!(!shown.contains(secret), "{secret}");
        }
    }

    #[test]
    fn ttl_words_match_the_legacy_strings() {
        assert_eq!(ttl_words(1440), "24 hours");
        assert_eq!(ttl_words(60), "1 hour");
        assert_eq!(ttl_words(10), "10 minutes");
    }
}
