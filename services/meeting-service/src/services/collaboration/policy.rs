//! Collaboration policy (Part F §24, §30, §53, §55).
//!
//! Effective policy = meeting-type defaults → space overrides
//! (`meeting_spaces.collaboration_policy`) → conference overrides
//! (`conference_sessions.collaboration_policy`). Overrides are sparse
//! ([`CollaborationPolicyPatch`]); the effective value is always complete. Policy can
//! only *narrow* what a role may do: media publication still requires the role grant
//! from `MeetingPermissionService` (§55).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::domain::MeetingType;

/// Who may publish a screen share (on top of the `screenshare.publish` role grant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScreenSharePolicy {
    /// OWNER / HOST / CO_HOST only.
    HostOnly,
    /// Host hierarchy + MODERATOR + PRESENTER.
    PresentersOnly,
    /// Every role whose current permissions allow screen sharing.
    AllInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChatSendMode {
    Everyone,
    /// Announcements only: only roles with `chat.moderate` may post.
    HostsOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrivateChatMode {
    Everyone,
    /// Private messages may only be exchanged with moderators.
    HostsOnly,
    Disabled,
}

/// Breakout recording metadata only — recording itself is Part G.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BreakoutRecordingPolicy {
    InheritMainPolicy,
    Disabled,
    SeparateAllowed,
}

impl BreakoutRecordingPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            BreakoutRecordingPolicy::InheritMainPolicy => "INHERIT_MAIN_POLICY",
            BreakoutRecordingPolicy::Disabled => "DISABLED",
            BreakoutRecordingPolicy::SeparateAllowed => "SEPARATE_ALLOWED",
        }
    }
}

/// Explicit pre-join consent kinds (`meeting_consents.consent_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConsentKind {
    Recording,
    Transcription,
    AiProcessing,
    /// Part G: sharing meeting data with a third party (independent of the others).
    DataSharing,
    HealthcareTerms,
    PrivacyNotice,
}

impl ConsentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConsentKind::Recording => "RECORDING",
            ConsentKind::Transcription => "TRANSCRIPTION",
            ConsentKind::AiProcessing => "AI_PROCESSING",
            ConsentKind::DataSharing => "DATA_SHARING",
            ConsentKind::HealthcareTerms => "HEALTHCARE_TERMS",
            ConsentKind::PrivacyNotice => "PRIVACY_NOTICE",
        }
    }
}

pub const DEFAULT_REACTIONS: &[&str] = &[
    "thumbs_up",
    "clap",
    "heart",
    "laugh",
    "surprised",
    "celebrate",
];

/// Complete effective collaboration policy of a conference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationPolicy {
    pub screen_share_policy: ScreenSharePolicy,
    /// Conference-wide microphone lock for every role without `participant.mute`.
    pub audio_locked: bool,
    /// Conference-wide camera lock for every role without `participant.mute`.
    pub video_locked: bool,
    pub max_spotlights: u8,
    pub chat_enabled: bool,
    pub chat_send_mode: ChatSendMode,
    pub private_chat_mode: PrivateChatMode,
    pub breakout_chat_enabled: bool,
    /// Whether participants inside a breakout still see (read-only) main chat.
    pub main_chat_visible_in_breakouts: bool,
    pub reactions_enabled: bool,
    pub allowed_reactions: Vec<String>,
    pub hand_raise_enabled: bool,
    pub qna_enabled: bool,
    pub qna_anonymous_allowed: bool,
    pub polls_enabled: bool,
    pub max_breakouts: u16,
    pub breakout_recording_policy: BreakoutRecordingPolicy,
    /// Consents an attendee must grant before admission (pre-join contract).
    pub required_consents: Vec<ConsentKind>,
    pub consent_version: String,
    /// Show an AI-processing notice before join (no AI runs in Part F).
    pub ai_notice: bool,
}

impl CollaborationPolicy {
    /// Meeting-type defaults. Webinar-style meetings keep attendees subscribe-only
    /// (enforced through the VIEWER default role) and restrict private chat and screen
    /// sharing, while Q&A, reactions, polls and hand raise stay available.
    pub fn defaults(meeting_type: MeetingType) -> Self {
        let webinar = matches!(meeting_type, MeetingType::Webinar | MeetingType::TownHall);
        Self {
            screen_share_policy: if webinar {
                ScreenSharePolicy::PresentersOnly
            } else {
                ScreenSharePolicy::AllInteractive
            },
            audio_locked: false,
            video_locked: false,
            max_spotlights: 1,
            chat_enabled: true,
            chat_send_mode: ChatSendMode::Everyone,
            private_chat_mode: if webinar {
                PrivateChatMode::HostsOnly
            } else {
                PrivateChatMode::Everyone
            },
            breakout_chat_enabled: true,
            main_chat_visible_in_breakouts: false,
            reactions_enabled: true,
            allowed_reactions: DEFAULT_REACTIONS.iter().map(|s| s.to_string()).collect(),
            hand_raise_enabled: true,
            qna_enabled: true,
            qna_anonymous_allowed: true,
            polls_enabled: true,
            max_breakouts: 50,
            breakout_recording_policy: BreakoutRecordingPolicy::InheritMainPolicy,
            required_consents: vec![],
            consent_version: "v1".into(),
            ai_notice: false,
        }
    }

    pub fn apply(mut self, p: &CollaborationPolicyPatch) -> Self {
        macro_rules! set {
            ($($f:ident),+) => { $( if let Some(v) = &p.$f { self.$f = v.clone(); } )+ };
        }
        set!(
            screen_share_policy,
            audio_locked,
            video_locked,
            max_spotlights,
            chat_enabled,
            chat_send_mode,
            private_chat_mode,
            breakout_chat_enabled,
            main_chat_visible_in_breakouts,
            reactions_enabled,
            allowed_reactions,
            hand_raise_enabled,
            qna_enabled,
            qna_anonymous_allowed,
            polls_enabled,
            max_breakouts,
            breakout_recording_policy,
            required_consents,
            consent_version,
            ai_notice
        );
        self
    }

    /// Effective policy from the persisted sparse overrides.
    pub fn effective(
        meeting_type: MeetingType,
        space: &serde_json::Value,
        conference: &serde_json::Value,
    ) -> Self {
        Self::defaults(meeting_type)
            .apply(&CollaborationPolicyPatch::from_json(space))
            .apply(&CollaborationPolicyPatch::from_json(conference))
    }

    pub fn reaction_allowed(&self, reaction: &str) -> bool {
        valid_reaction_key(reaction) && self.allowed_reactions.iter().any(|r| r == reaction)
    }
}

/// Sparse override. Unknown fields are rejected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollaborationPolicyPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_share_policy: Option<ScreenSharePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_locked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_locked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_spotlights: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_send_mode: Option<ChatSendMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_chat_mode: Option<PrivateChatMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakout_chat_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_chat_visible_in_breakouts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reactions_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_reactions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hand_raise_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qna_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qna_anonymous_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub polls_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_breakouts: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakout_recording_policy: Option<BreakoutRecordingPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_consents: Option<Vec<ConsentKind>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_notice: Option<bool>,
}

pub fn valid_reaction_key(key: &str) -> bool {
    (1..=32).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl CollaborationPolicyPatch {
    /// Persisted overrides are validated on write; unreadable JSON degrades to "no
    /// override" rather than failing every request.
    pub fn from_json(value: &serde_json::Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Later fields win.
    pub fn merged(&self, newer: &CollaborationPolicyPatch) -> Self {
        let mut a = self.to_json();
        if let (Some(map), serde_json::Value::Object(b)) = (a.as_object_mut(), newer.to_json()) {
            for (k, v) in b {
                map.insert(k, v);
            }
        }
        Self::from_json(&a)
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(n) = self.max_spotlights {
            if !(1..=9).contains(&n) {
                return Err("maxSpotlights must be between 1 and 9".into());
            }
        }
        if let Some(n) = self.max_breakouts {
            if !(1..=100).contains(&n) {
                return Err("maxBreakouts must be between 1 and 100".into());
            }
        }
        if let Some(list) = &self.allowed_reactions {
            if list.is_empty() || list.len() > 32 {
                return Err("allowedReactions must contain 1–32 entries".into());
            }
            if let Some(bad) = list.iter().find(|r| !valid_reaction_key(r)) {
                return Err(format!("reaction '{bad}' must match [a-z0-9_]{{1,32}}"));
            }
        }
        if let Some(v) = &self.consent_version {
            if v.is_empty()
                || v.len() > 32
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            {
                return Err("consentVersion must be 1–32 characters [A-Za-z0-9._-]".into());
            }
        }
        if let Some(c) = &self.required_consents {
            if c.len() > 5 {
                return Err("requiredConsents has too many entries".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn webinar_defaults_are_restrictive() {
        let w = CollaborationPolicy::defaults(MeetingType::Webinar);
        assert_eq!(w.screen_share_policy, ScreenSharePolicy::PresentersOnly);
        assert_eq!(w.private_chat_mode, PrivateChatMode::HostsOnly);
        assert!(w.qna_enabled && w.reactions_enabled && w.polls_enabled && w.hand_raise_enabled);
        let s = CollaborationPolicy::defaults(MeetingType::Standard);
        assert_eq!(s.screen_share_policy, ScreenSharePolicy::AllInteractive);
    }

    #[test]
    fn precedence_space_then_conference() {
        let p = CollaborationPolicy::effective(
            MeetingType::Standard,
            &json!({ "chatEnabled": false, "maxSpotlights": 3 }),
            &json!({ "chatEnabled": true, "audioLocked": true }),
        );
        assert!(p.chat_enabled, "conference override wins");
        assert_eq!(p.max_spotlights, 3, "space override kept");
        assert!(p.audio_locked);
    }

    #[test]
    fn patch_rejects_unknown_fields_and_bad_values() {
        assert!(
            serde_json::from_value::<CollaborationPolicyPatch>(json!({ "godMode": true })).is_err()
        );
        let bad = CollaborationPolicyPatch {
            allowed_reactions: Some(vec!["<script>".into()]),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
        let bad = CollaborationPolicyPatch {
            max_spotlights: Some(0),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
        assert!(CollaborationPolicyPatch::default().validate().is_ok());
    }

    #[test]
    fn merge_keeps_older_fields() {
        let a = CollaborationPolicyPatch {
            chat_enabled: Some(false),
            ..Default::default()
        };
        let b = CollaborationPolicyPatch {
            audio_locked: Some(true),
            ..Default::default()
        };
        let m = a.merged(&b);
        assert_eq!(m.chat_enabled, Some(false));
        assert_eq!(m.audio_locked, Some(true));
    }

    #[test]
    fn reactions_validated_against_policy() {
        let p = CollaborationPolicy::defaults(MeetingType::Standard);
        assert!(p.reaction_allowed("clap"));
        assert!(!p.reaction_allowed("poo"));
        assert!(!p.reaction_allowed("CLAP"));
        assert!(!p.reaction_allowed(""));
    }
}
