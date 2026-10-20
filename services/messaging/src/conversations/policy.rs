//! Effective messaging policy (Part V D.4 / D.6).
//!
//! Effective policy = explicit PLATFORM defaults ([`platform_defaults`], versioned by
//! [`PLATFORM_POLICY_VERSION`]) merged with the application environment's validated,
//! versioned overrides ([`ApplicationPolicy`]). Resolution is a pure function; its result
//! is canonicalized (sorted keys) and stored as an immutable, content-addressed snapshot
//! that every conversation references, so a conversation's policy can be reproduced.
//!
//! Platform defaults are deliberately conservative and assume nothing about features that
//! do not exist yet: PRIVATE_E2EE is the default for every context class — direct, group,
//! meeting, transaction, consultation, support, enterprise and developer-defined (Part V
//! Appendix G: "unless tenant policy requires managed mode"); MANAGED is NEVER available
//! unless the application's policy enables it for a context, and never becomes a default
//! by itself; replies, threads, reactions, pins, attachments, moderation, retention,
//! presence and read receipts are unset / disabled until their workstreams; legal hold is
//! eligible only where MANAGED is allowed; the SMS Bridge is never eligible.
//!
//! Platform policy version 2 (Part V specification review): SUPPORT defaults to
//! PRIVATE_E2EE like every other context (version 1 left it without a default) and the
//! snapshot names the message features of §28 (`messages`).
//!
//! Platform policy version 3 (Parts E–G): the features now implemented carry explicit,
//! overridable values — attachments (OFF by default), editing windows, privacy of
//! presence / typing / read receipts (on for co-members, no last-seen), retention (kept
//! until deleted; immediate purge after deletion), moderation and reporting, abuse limits
//! — and contexts gain regulated controls (§92): `regulated`, `centralArchiveRequired`
//! (MANAGED only), `attachmentsEnabled`, `reportingEnabled`, `exportsEnabled` (MANAGED
//! only) and `retentionDays`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::ContextClass;
use crate::{
    db,
    domain::SecurityProfile,
    error::{Error, ErrorCode, Result},
};

/// Version of the platform defaults below.
pub const PLATFORM_POLICY_VERSION: i32 = 3;
/// Version of the policy document schema.
pub const POLICY_SCHEMA_VERSION: i32 = 1;
/// Version of the security policy persisted with every conversation (profile rules).
pub const SECURITY_POLICY_VERSION: i32 = 1;

/// The effective policy of one context class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPolicy {
    pub enabled: bool,
    pub allowed_profiles: Vec<SecurityProfile>,
    /// `None`: the creator must name a profile (and it must be allowed).
    pub default_profile: Option<SecurityProfile>,
    /// `None`: unset (the platform bound applies).
    pub max_members: Option<i64>,
    /// `None`: no context expiry.
    pub max_lifetime_seconds: Option<i64>,
    pub reopen_allowed: bool,
    pub legal_hold_eligible: bool,
    pub sms_bridge_eligible: bool,
    /// Regulated context (§92): MANAGED conversations need an explicit disclosure
    /// acknowledgement at creation; exports / reporting follow the flags below.
    pub regulated: bool,
    /// Centralized server-side archival is required: only MANAGED is allowed (never an
    /// E2EE conversation whose content the server cannot keep).
    pub central_archive_required: bool,
    /// `None`: the environment's attachment setting applies.
    pub attachments_enabled: Option<bool>,
    pub reporting_enabled: bool,
    /// Server-side exports (MANAGED only, never E2EE plaintext).
    pub exports_enabled: bool,
    /// `None`: the environment's retention applies.
    pub retention_days: Option<i64>,
}

/// Attachment policy of an environment (§69–§80). Sizes in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPolicy {
    pub enabled: bool,
    pub max_bytes: i64,
    pub max_per_message: i64,
    pub allowed_kinds: Vec<String>,
    /// MANAGED attachments must pass the self-hosted scanner before they are usable.
    pub managed_scan_required: bool,
    pub max_voice_note_seconds: i64,
    pub max_video_seconds: i64,
    /// `None`: only the deployment's limit applies.
    pub storage_budget_bytes: Option<i64>,
    pub daily_upload_bytes: Option<i64>,
    pub max_concurrent_uploads: i64,
}

/// Privacy of ephemeral signals (§62–§63).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyPolicy {
    pub presence: bool,
    pub typing_indicators: bool,
    pub read_receipts: bool,
    pub last_seen: bool,
}

/// Retention (§89–§90). `None` days: kept until deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicy {
    pub message_days: Option<i64>,
    /// Delay between a user-visible deletion and the irreversible purge.
    pub deleted_purge_grace_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModerationPolicy {
    pub enabled: bool,
    pub reports_enabled: bool,
    /// MANAGED only: a report may reference server-visible content (never E2EE).
    pub managed_evidence_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditingPolicy {
    /// `None`: no window.
    pub edit_window_seconds: Option<i64>,
    pub delete_window_seconds: Option<i64>,
}

/// Abuse limits (§88), per acting principal unless stated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AbusePolicy {
    pub messages_per_minute: i64,
    pub new_direct_contacts_per_hour: i64,
    pub reports_per_hour: i64,
    pub uploads_per_hour: i64,
    /// MANAGED only: links per principal per hour (content is visible by design).
    pub managed_links_per_hour: i64,
}

/// The effective policy of an application environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectivePolicy {
    pub schema_version: i32,
    pub platform_policy_version: i32,
    pub application_policy_version: i64,
    pub contexts: BTreeMap<ContextClass, ContextPolicy>,
    pub sending_enabled: bool,
    /// Message features not implemented yet: present, explicit, and unset.
    pub messages: Value,
    pub attachments: AttachmentPolicy,
    pub editing: EditingPolicy,
    pub privacy: PrivacyPolicy,
    pub retention: RetentionPolicy,
    pub moderation: ModerationPolicy,
    pub abuse: AbusePolicy,
}

/// Attachment kinds (generic; no product vocabulary).
pub const ATTACHMENT_KINDS: &[&str] = &["IMAGE", "DOCUMENT", "FILE", "VOICE_NOTE", "VIDEO"];

/// The platform defaults (explicit; see the module documentation).
pub fn platform_defaults() -> EffectivePolicy {
    let contexts = ContextClass::ALL
        .iter()
        .map(|c| {
            (
                *c,
                ContextPolicy {
                    enabled: true,
                    allowed_profiles: vec![SecurityProfile::PrivateE2ee],
                    default_profile: Some(SecurityProfile::PrivateE2ee),
                    max_members: if *c == ContextClass::Direct {
                        Some(2)
                    } else {
                        None
                    },
                    max_lifetime_seconds: None,
                    reopen_allowed: true,
                    legal_hold_eligible: false,
                    sms_bridge_eligible: false,
                    regulated: false,
                    central_archive_required: false,
                    attachments_enabled: None,
                    reporting_enabled: true,
                    exports_enabled: false,
                    retention_days: None,
                },
            )
        })
        .collect();
    EffectivePolicy {
        schema_version: POLICY_SCHEMA_VERSION,
        platform_policy_version: PLATFORM_POLICY_VERSION,
        application_policy_version: 0,
        contexts,
        sending_enabled: true,
        messages: json!({ "replies": null, "threads": null, "reactions": null, "pins": null }),
        // Attachments exist but are OFF until an application enables them; no commercial
        // storage budget is invented here (deployment limits apply).
        attachments: AttachmentPolicy {
            enabled: false,
            max_bytes: 25 * 1024 * 1024,
            max_per_message: 10,
            allowed_kinds: ATTACHMENT_KINDS.iter().map(|k| k.to_string()).collect(),
            managed_scan_required: true,
            max_voice_note_seconds: 900,
            max_video_seconds: 600,
            storage_budget_bytes: None,
            daily_upload_bytes: None,
            max_concurrent_uploads: 10,
        },
        editing: EditingPolicy {
            edit_window_seconds: None,
            delete_window_seconds: None,
        },
        // Privacy-respecting defaults: presence only to co-members, no last-seen.
        privacy: PrivacyPolicy {
            presence: true,
            typing_indicators: true,
            read_receipts: true,
            last_seen: false,
        },
        retention: RetentionPolicy {
            message_days: None,
            deleted_purge_grace_seconds: 0,
        },
        moderation: ModerationPolicy {
            enabled: true,
            reports_enabled: true,
            managed_evidence_allowed: false,
        },
        abuse: AbusePolicy {
            messages_per_minute: 120,
            new_direct_contacts_per_hour: 30,
            reports_per_hour: 20,
            uploads_per_hour: 120,
            managed_links_per_hour: 60,
        },
    }
}

/// An application's override of one context class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_profiles: Option<Vec<SecurityProfile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<SecurityProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_members: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopen_allowed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regulated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub central_archive_required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exports_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<i64>,
}

macro_rules! overrides {
    ($(#[$m:meta])* $name:ident { $($field:ident : $ty:ty),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name {
            $(#[serde(default, skip_serializing_if = "Option::is_none")] pub $field: Option<$ty>,)+
        }
    };
}

overrides!(AttachmentOverride {
    enabled: bool,
    max_bytes: i64,
    max_per_message: i64,
    allowed_kinds: Vec<String>,
    managed_scan_required: bool,
    max_voice_note_seconds: i64,
    max_video_seconds: i64,
    storage_budget_bytes: i64,
    daily_upload_bytes: i64,
    max_concurrent_uploads: i64,
});
overrides!(PrivacyOverride {
    presence: bool,
    typing_indicators: bool,
    read_receipts: bool,
    last_seen: bool,
});
overrides!(RetentionOverride {
    message_days: i64,
    deleted_purge_grace_seconds: i64,
});
overrides!(ModerationOverride {
    enabled: bool,
    reports_enabled: bool,
    managed_evidence_allowed: bool,
});
overrides!(EditingOverride {
    edit_window_seconds: i64,
    delete_window_seconds: i64,
});
overrides!(AbuseOverride {
    messages_per_minute: i64,
    new_direct_contacts_per_hour: i64,
    reports_per_hour: i64,
    uploads_per_hour: i64,
    managed_links_per_hour: i64,
});

/// An application environment's policy overrides (what `PUT /v1/messaging/policy` sets).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationPolicy {
    #[serde(default)]
    pub contexts: BTreeMap<ContextClass, ContextOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sending_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<AttachmentOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<PrivacyOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moderation: Option<ModerationOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editing: Option<EditingOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abuse: Option<AbuseOverride>,
}

const MAX_LIFETIME: i64 = 10 * 365 * 86_400;
const MAX_RETENTION_DAYS: i64 = 36_500;

fn within(v: Option<i64>, min: i64, max: i64, what: &str) -> std::result::Result<(), String> {
    match v {
        Some(n) if !(min..=max).contains(&n) => {
            Err(format!("{what} must be between {min} and {max}"))
        }
        _ => Ok(()),
    }
}

impl ApplicationPolicy {
    /// Validation independent of the defaults (bounds, consistency).
    pub fn validate(&self) -> std::result::Result<(), String> {
        for (class, o) in &self.contexts {
            if let Some(profiles) = &o.allowed_profiles {
                let mut seen = profiles.clone();
                seen.sort_by_key(|p| p.as_str());
                seen.dedup();
                if seen.len() != profiles.len() {
                    return Err(format!(
                        "{}: allowedProfiles has duplicates",
                        class.as_str()
                    ));
                }
            }
            if let Some(m) = o.max_members {
                if *class == ContextClass::Direct && m != 2 {
                    return Err("DIRECT conversations always have exactly 2 members".into());
                }
                if !(2..=100_000).contains(&m) {
                    return Err(format!(
                        "{}: maxMembers must be between 2 and 100000",
                        class.as_str()
                    ));
                }
            }
            if let Some(l) = o.max_lifetime_seconds {
                if !(60..=MAX_LIFETIME).contains(&l) {
                    return Err(format!(
                        "{}: maxLifetimeSeconds must be between 60 and {MAX_LIFETIME}",
                        class.as_str()
                    ));
                }
            }
            within(
                o.retention_days,
                1,
                MAX_RETENTION_DAYS,
                &format!("{}: retentionDays", class.as_str()),
            )?;
        }
        if let Some(a) = &self.attachments {
            within(
                a.max_bytes,
                1,
                4 * 1024 * 1024 * 1024,
                "attachments.maxBytes",
            )?;
            within(a.max_per_message, 1, 100, "attachments.maxPerMessage")?;
            within(
                a.max_voice_note_seconds,
                1,
                86_400,
                "attachments.maxVoiceNoteSeconds",
            )?;
            within(
                a.max_video_seconds,
                1,
                86_400,
                "attachments.maxVideoSeconds",
            )?;
            within(
                a.storage_budget_bytes,
                0,
                i64::MAX,
                "attachments.storageBudgetBytes",
            )?;
            within(
                a.daily_upload_bytes,
                0,
                i64::MAX,
                "attachments.dailyUploadBytes",
            )?;
            within(
                a.max_concurrent_uploads,
                1,
                1_000,
                "attachments.maxConcurrentUploads",
            )?;
            if let Some(kinds) = &a.allowed_kinds {
                if kinds
                    .iter()
                    .any(|k| !ATTACHMENT_KINDS.contains(&k.as_str()))
                {
                    return Err(format!(
                        "attachments.allowedKinds must be among {ATTACHMENT_KINDS:?}"
                    ));
                }
            }
        }
        if let Some(r) = &self.retention {
            within(
                r.message_days,
                1,
                MAX_RETENTION_DAYS,
                "retention.messageDays",
            )?;
            within(
                r.deleted_purge_grace_seconds,
                0,
                365 * 86_400,
                "retention.deletedPurgeGraceSeconds",
            )?;
        }
        if let Some(e) = &self.editing {
            within(
                e.edit_window_seconds,
                1,
                MAX_LIFETIME,
                "editing.editWindowSeconds",
            )?;
            within(
                e.delete_window_seconds,
                1,
                MAX_LIFETIME,
                "editing.deleteWindowSeconds",
            )?;
        }
        if let Some(a) = &self.abuse {
            within(a.messages_per_minute, 1, 100_000, "abuse.messagesPerMinute")?;
            within(
                a.new_direct_contacts_per_hour,
                1,
                100_000,
                "abuse.newDirectContactsPerHour",
            )?;
            within(a.reports_per_hour, 1, 10_000, "abuse.reportsPerHour")?;
            within(a.uploads_per_hour, 1, 100_000, "abuse.uploadsPerHour")?;
            within(
                a.managed_links_per_hour,
                1,
                100_000,
                "abuse.managedLinksPerHour",
            )?;
        }
        Ok(())
    }
}

/// Deterministic resolution of the effective policy.
pub fn resolve(
    overrides: &ApplicationPolicy,
    application_version: i64,
) -> std::result::Result<EffectivePolicy, String> {
    overrides.validate()?;
    let mut p = platform_defaults();
    p.application_policy_version = application_version;
    if let Some(s) = overrides.sending_enabled {
        p.sending_enabled = s;
    }
    for (class, o) in &overrides.contexts {
        let c = p.contexts.get_mut(class).expect("every class has defaults");
        if let Some(e) = o.enabled {
            c.enabled = e;
        }
        if let Some(allowed) = &o.allowed_profiles {
            let mut allowed = allowed.clone();
            allowed.sort_by_key(|p| p.as_str());
            c.allowed_profiles = allowed;
            // A default that is no longer allowed is dropped (never a silent fallback).
            if c.default_profile
                .is_some_and(|d| !c.allowed_profiles.contains(&d))
            {
                c.default_profile = None;
            }
        }
        if let Some(d) = o.default_profile {
            if !c.allowed_profiles.contains(&d) {
                return Err(format!(
                    "{}: defaultProfile must be one of allowedProfiles",
                    class.as_str()
                ));
            }
            c.default_profile = Some(d);
        }
        if o.max_members.is_some() {
            c.max_members = o.max_members;
        }
        if o.max_lifetime_seconds.is_some() {
            c.max_lifetime_seconds = o.max_lifetime_seconds;
        }
        if let Some(r) = o.reopen_allowed {
            c.reopen_allowed = r;
        }
        if let Some(r) = o.regulated {
            c.regulated = r;
        }
        if let Some(r) = o.central_archive_required {
            c.central_archive_required = r;
        }
        if c.central_archive_required && c.allowed_profiles != vec![SecurityProfile::Managed] {
            return Err(format!(
                "{}: centralArchiveRequired allows MANAGED only (allowedProfiles must be [MANAGED]); \
                 an E2EE conversation cannot be archived by the server",
                class.as_str()
            ));
        }
        if o.attachments_enabled.is_some() {
            c.attachments_enabled = o.attachments_enabled;
        }
        if let Some(r) = o.reporting_enabled {
            c.reporting_enabled = r;
        }
        if let Some(e) = o.exports_enabled {
            if e && !c.allowed_profiles.contains(&SecurityProfile::Managed) {
                return Err(format!(
                    "{}: exportsEnabled requires MANAGED (the server cannot export E2EE plaintext)",
                    class.as_str()
                ));
            }
            c.exports_enabled = e;
        }
        if o.retention_days.is_some() {
            c.retention_days = o.retention_days;
        }
        c.legal_hold_eligible = c.allowed_profiles.contains(&SecurityProfile::Managed);
        c.sms_bridge_eligible = false;
    }
    if let Some(a) = &overrides.attachments {
        let t = &mut p.attachments;
        if let Some(v) = a.enabled {
            t.enabled = v;
        }
        if let Some(v) = a.max_bytes {
            t.max_bytes = v;
        }
        if let Some(v) = a.max_per_message {
            t.max_per_message = v;
        }
        if let Some(v) = &a.allowed_kinds {
            let mut v = v.clone();
            v.sort();
            v.dedup();
            t.allowed_kinds = v;
        }
        if let Some(v) = a.managed_scan_required {
            t.managed_scan_required = v;
        }
        if let Some(v) = a.max_voice_note_seconds {
            t.max_voice_note_seconds = v;
        }
        if let Some(v) = a.max_video_seconds {
            t.max_video_seconds = v;
        }
        if a.storage_budget_bytes.is_some() {
            t.storage_budget_bytes = a.storage_budget_bytes;
        }
        if a.daily_upload_bytes.is_some() {
            t.daily_upload_bytes = a.daily_upload_bytes;
        }
        if let Some(v) = a.max_concurrent_uploads {
            t.max_concurrent_uploads = v;
        }
    }
    if let Some(o) = &overrides.privacy {
        let t = &mut p.privacy;
        if let Some(v) = o.presence {
            t.presence = v;
        }
        if let Some(v) = o.typing_indicators {
            t.typing_indicators = v;
        }
        if let Some(v) = o.read_receipts {
            t.read_receipts = v;
        }
        if let Some(v) = o.last_seen {
            t.last_seen = v;
        }
    }
    if let Some(o) = &overrides.retention {
        if o.message_days.is_some() {
            p.retention.message_days = o.message_days;
        }
        if let Some(v) = o.deleted_purge_grace_seconds {
            p.retention.deleted_purge_grace_seconds = v;
        }
    }
    if let Some(o) = &overrides.moderation {
        let t = &mut p.moderation;
        if let Some(v) = o.enabled {
            t.enabled = v;
        }
        if let Some(v) = o.reports_enabled {
            t.reports_enabled = v;
        }
        if let Some(v) = o.managed_evidence_allowed {
            t.managed_evidence_allowed = v;
        }
    }
    if let Some(o) = &overrides.editing {
        if o.edit_window_seconds.is_some() {
            p.editing.edit_window_seconds = o.edit_window_seconds;
        }
        if o.delete_window_seconds.is_some() {
            p.editing.delete_window_seconds = o.delete_window_seconds;
        }
    }
    if let Some(o) = &overrides.abuse {
        let t = &mut p.abuse;
        if let Some(v) = o.messages_per_minute {
            t.messages_per_minute = v;
        }
        if let Some(v) = o.new_direct_contacts_per_hour {
            t.new_direct_contacts_per_hour = v;
        }
        if let Some(v) = o.reports_per_hour {
            t.reports_per_hour = v;
        }
        if let Some(v) = o.uploads_per_hour {
            t.uploads_per_hour = v;
        }
        if let Some(v) = o.managed_links_per_hour {
            t.managed_links_per_hour = v;
        }
    }
    Ok(p)
}

/// The CURRENT effective policy of an environment (operational features — attachments,
/// privacy, retention, moderation, abuse — follow the current policy; a conversation's
/// snapshot fixes its security profile and creation rules).
pub async fn current<C: ConnectionTrait>(db: &C, environment_id: Uuid) -> Result<EffectivePolicy> {
    let (overrides, version) = load_application_policy(db, environment_id, false).await?;
    resolve(&overrides, version).map_err(|_| Error::code(ErrorCode::InternalError))
}

/// JSON with recursively sorted object keys (independent of serde_json feature flags).
pub fn canonical_json(value: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string"));
                    out.push(':');
                    write(&map[*k], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).expect("scalar")),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

impl EffectivePolicy {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("policy serializes")
    }

    /// SHA-256 (hex) of the canonical document.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(canonical_json(&self.to_value()).as_bytes()))
    }

    pub fn context(&self, class: ContextClass) -> &ContextPolicy {
        &self.contexts[&class]
    }
}

#[derive(Debug, Clone, FromQueryResult)]
struct PolicyRow {
    version: i64,
    document: Value,
}

/// The application environment's stored overrides and their version (0 = none).
pub async fn load_application_policy<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    lock: bool,
) -> Result<(ApplicationPolicy, i64)> {
    let row: Option<PolicyRow> = db::one(
        db,
        &format!(
            "SELECT version, document FROM messaging_application_policies WHERE environment_id = $1{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        vec![environment_id.into()],
    )
    .await?;
    match row {
        None => Ok((ApplicationPolicy::default(), 0)),
        Some(r) => {
            let policy = serde_json::from_value(r.document)
                .map_err(|_| Error::code(ErrorCode::InternalError))?;
            Ok((policy, r.version))
        }
    }
}

/// A stored snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: Uuid,
    pub digest: String,
    pub policy: EffectivePolicy,
}

#[derive(FromQueryResult)]
struct SnapshotId {
    id: Uuid,
}

/// The current effective policy of `environment_id`, stored as an immutable snapshot
/// (created once per distinct document; concurrent callers converge on one row).
pub async fn current_snapshot<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Snapshot> {
    let (overrides, version) = load_application_policy(db, environment_id, false).await?;
    let policy = resolve(&overrides, version).map_err(|_| Error::code(ErrorCode::InternalError))?;
    let digest = policy.digest();
    db::exec(
        db,
        "INSERT INTO messaging_policy_snapshots (id, environment_id, digest, platform_policy_version, \
           application_policy_version, document, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (environment_id, digest) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            environment_id.into(),
            digest.clone().into(),
            PLATFORM_POLICY_VERSION.into(),
            version.into(),
            policy.to_value().into(),
            now.into(),
        ],
    )
    .await?;
    let row: SnapshotId = db::one(
        db,
        "SELECT id FROM messaging_policy_snapshots WHERE environment_id = $1 AND digest = $2",
        vec![environment_id.into(), digest.clone().into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    Ok(Snapshot {
        id: row.id,
        digest,
        policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_defaults_are_e2ee_where_eligible_and_never_managed() {
        let p = platform_defaults();
        assert_eq!(p.contexts.len(), 8);
        for (class, c) in &p.contexts {
            assert!(
                !c.allowed_profiles.contains(&SecurityProfile::Managed),
                "{class:?}"
            );
            assert!(!c.sms_bridge_eligible && !c.legal_hold_eligible);
            assert_eq!(
                c.default_profile,
                Some(SecurityProfile::PrivateE2ee),
                "{class:?}: PRIVATE_E2EE is the default for every context (Appendix G)"
            );
            assert_eq!(c.allowed_profiles, vec![SecurityProfile::PrivateE2ee]);
        }
        assert_eq!(p.context(ContextClass::Direct).max_members, Some(2));
    }

    #[test]
    fn resolution_is_deterministic_and_validated() {
        let mut o = ApplicationPolicy::default();
        o.contexts.insert(
            ContextClass::Enterprise,
            ContextOverride {
                allowed_profiles: Some(vec![
                    SecurityProfile::Managed,
                    SecurityProfile::PrivateE2ee,
                ]),
                default_profile: Some(SecurityProfile::Managed),
                ..Default::default()
            },
        );
        let a = resolve(&o, 3).unwrap();
        let b = resolve(&o, 3).unwrap();
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), resolve(&o, 4).unwrap().digest());
        assert_ne!(a.digest(), platform_defaults().digest());
        let e = a.context(ContextClass::Enterprise);
        assert_eq!(e.default_profile, Some(SecurityProfile::Managed));
        assert!(e.legal_hold_eligible && !e.sms_bridge_eligible);
        // Profile order in the override does not change the digest.
        o.contexts
            .get_mut(&ContextClass::Enterprise)
            .unwrap()
            .allowed_profiles = Some(vec![SecurityProfile::PrivateE2ee, SecurityProfile::Managed]);
        assert_eq!(resolve(&o, 3).unwrap().digest(), a.digest());
        // Removing E2EE from a context drops its default instead of falling back.
        let mut only_managed = ApplicationPolicy::default();
        only_managed.contexts.insert(
            ContextClass::Group,
            ContextOverride {
                allowed_profiles: Some(vec![SecurityProfile::Managed]),
                ..Default::default()
            },
        );
        assert_eq!(
            resolve(&only_managed, 1)
                .unwrap()
                .context(ContextClass::Group)
                .default_profile,
            None
        );
        // Invalid overrides.
        for bad in [
            ContextOverride {
                default_profile: Some(SecurityProfile::Managed),
                ..Default::default()
            },
            ContextOverride {
                max_members: Some(1),
                ..Default::default()
            },
            ContextOverride {
                max_lifetime_seconds: Some(5),
                ..Default::default()
            },
            ContextOverride {
                allowed_profiles: Some(vec![SecurityProfile::Managed, SecurityProfile::Managed]),
                ..Default::default()
            },
        ] {
            let mut p = ApplicationPolicy::default();
            p.contexts.insert(ContextClass::Group, bad.clone());
            assert!(resolve(&p, 1).is_err(), "{bad:?}");
        }
        let mut direct = ApplicationPolicy::default();
        direct.contexts.insert(
            ContextClass::Direct,
            ContextOverride {
                max_members: Some(10),
                ..Default::default()
            },
        );
        assert!(resolve(&direct, 1).is_err());
    }

    #[test]
    fn version_3_feature_policies_are_explicit_and_regulated_contexts_never_hold_e2ee_archives() {
        let p = platform_defaults();
        assert_eq!(p.platform_policy_version, 3);
        assert!(!p.attachments.enabled, "attachments are off until enabled");
        assert!(
            p.attachments.storage_budget_bytes.is_none(),
            "no invented commercial budget"
        );
        assert!(p.privacy.presence && !p.privacy.last_seen);
        assert!(p.retention.message_days.is_none());
        // centralArchiveRequired allows MANAGED only.
        let mut o = ApplicationPolicy::default();
        o.contexts.insert(
            ContextClass::Consultation,
            ContextOverride {
                central_archive_required: Some(true),
                ..Default::default()
            },
        );
        assert!(
            resolve(&o, 1).is_err(),
            "an E2EE context cannot require server archival"
        );
        o.contexts
            .get_mut(&ContextClass::Consultation)
            .unwrap()
            .allowed_profiles = Some(vec![SecurityProfile::Managed]);
        o.contexts
            .get_mut(&ContextClass::Consultation)
            .unwrap()
            .regulated = Some(true);
        let r = resolve(&o, 1).unwrap();
        let c = r.context(ContextClass::Consultation);
        assert!(c.central_archive_required && c.regulated && c.legal_hold_eligible);
        assert_eq!(
            c.default_profile, None,
            "MANAGED is never a default by itself"
        );
        // Exports need MANAGED.
        let mut e = ApplicationPolicy::default();
        e.contexts.insert(
            ContextClass::Group,
            ContextOverride {
                exports_enabled: Some(true),
                ..Default::default()
            },
        );
        assert!(resolve(&e, 1).is_err());
        // Bounds of the feature overrides.
        let bad: ApplicationPolicy =
            serde_json::from_value(json!({ "attachments": { "allowedKinds": ["EXECUTABLE"] } }))
                .unwrap();
        assert!(resolve(&bad, 1).is_err());
        let bad: ApplicationPolicy =
            serde_json::from_value(json!({ "retention": { "messageDays": 0 } })).unwrap();
        assert!(resolve(&bad, 1).is_err());
        assert!(
            serde_json::from_value::<ApplicationPolicy>(json!({ "privacy": { "spy": true } }))
                .is_err()
        );
        let ok: ApplicationPolicy = serde_json::from_value(
            json!({ "privacy": { "presence": false }, "attachments": { "enabled": true } }),
        )
        .unwrap();
        let r = resolve(&ok, 2).unwrap();
        assert!(!r.privacy.presence && r.attachments.enabled);
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let a: Value =
            serde_json::from_str(r#"{"b":1,"a":{"d":[{"z":1,"y":2}],"c":null}}"#).unwrap();
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"c":null,"d":[{"y":2,"z":1}]},"b":1}"#
        );
    }
}
