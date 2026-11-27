//! The precedence engine (Document 4 §7.2): rules are evaluated top to bottom and the first
//! terminal rule wins. This module is pure (no I/O): the processor gathers the inputs
//! (owner answers, preferences, endpoints, rate counters) and records the outcome as the
//! notification's `policy_snapshot` for audit.
//!
//! | # | Rule | Effect |
//! |---|---|---|
//! | 1 | recipient not authorized | drop |
//! | 2 | operator hold (X4) | drop, except security |
//! | 3 | security-critical | deliver email + in-app; ignore everything below |
//! | 4 | block (X4) | drop for social categories |
//! | 5 | organization policy (D-7) | slot (no policy defined yet) |
//! | 6 | privacy (X4) | drop |
//! | 7 | category off | no push / email; in-app silent |
//! | 8 | channel off | remove that channel |
//! | 9 | contact / conversation mute | no push; in-app silent; MENTIONS only with `mentionsRecipient` (MANAGED); PRIVATE_E2EE treats MENTIONS as MUTED |
//! | 10 | quiet hours | defer push to the window end; email never deferred |
//! | 11 | no enabled push endpoint | no push |
//! | 12 | per-recipient push cap (30 / h) | collapse (no further push this hour) |

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    authorities::Restriction,
    catalogue::{Channel, PrefCategory},
    preferences::AccountPrefs,
};

pub const PUSH_CAP_PER_HOUR: i64 = 30;

/// Social categories subject to blocks (rule 4).
fn social(c: PrefCategory) -> bool {
    matches!(
        c,
        PrefCategory::DirectMessages
            | PrefCategory::GroupMessages
            | PrefCategory::IncomingCalls
            | PrefCategory::MissedCalls
            | PrefCategory::ContactRequests
            | PrefCategory::GroupInvitations
            | PrefCategory::Voicemail
    )
}

#[derive(Debug, Clone, Default)]
pub struct PolicyInput {
    pub category: Option<PrefCategory>,
    pub channels: Vec<Channel>,
    /// `false`: inactive / erased / suspended account, revoked membership (rule 1).
    pub recipient_authorized: bool,
    pub restriction: Option<Restriction>,
    pub prefs: AccountPrefs,
    /// `ALL` / `MENTIONS` / `MUTED` (conversation, or contact for contact events).
    pub mute_level: Option<String>,
    pub mentions_recipient: bool,
    pub private_e2ee: bool,
    pub quiet_window_end: Option<DateTime<Utc>>,
    pub has_push_endpoint: bool,
    pub pushes_last_hour: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChannelDecision {
    Deliver,
    Defer { until: DateTime<Utc> },
    Suppress { reason: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyOutcome {
    /// `Some(reason)`: nothing at all is delivered (rules 1, 2, 4, 6).
    pub dropped: Option<&'static str>,
    pub channels: Vec<(String, ChannelDecision)>,
    pub silent_inbox: bool,
    pub rule_trace: Vec<&'static str>,
    pub preference_version: i64,
}

impl PolicyOutcome {
    pub fn decision(&self, channel: Channel) -> Option<&ChannelDecision> {
        self.channels
            .iter()
            .find(|(c, _)| c == channel.as_str())
            .map(|(_, d)| d)
    }
}

pub fn evaluate(input: &PolicyInput) -> PolicyOutcome {
    let category = input.category.unwrap_or(PrefCategory::OrganizationUpdates);
    let security = matches!(category, PrefCategory::Security);
    let mut out = PolicyOutcome {
        dropped: None,
        channels: Vec::new(),
        silent_inbox: false,
        rule_trace: Vec::new(),
        preference_version: input.prefs.version,
    };
    let drop = |mut o: PolicyOutcome, rule: &'static str, reason: &'static str| {
        o.rule_trace.push(rule);
        o.dropped = Some(reason);
        o.channels.clear();
        o
    };
    // 1
    if !input.recipient_authorized {
        return drop(out, "1", "RECIPIENT_NOT_AUTHORIZED");
    }
    let r = input.restriction.unwrap_or_default();
    // 2
    if r.operator_hold && !security {
        return drop(out, "2", "OPERATOR_HOLD");
    }
    // 3
    if security {
        out.rule_trace.push("3");
        for c in &input.channels {
            if matches!(c, Channel::Email | Channel::InApp) {
                out.channels
                    .push((c.as_str().into(), ChannelDecision::Deliver));
            }
        }
        return out;
    }
    if matches!(category, PrefCategory::Operator) {
        out.rule_trace.push("operator");
        for c in &input.channels {
            out.channels
                .push((c.as_str().into(), ChannelDecision::Deliver));
        }
        return out;
    }
    // 4
    if r.blocked && social(category) {
        return drop(out, "4", "BLOCKED");
    }
    // 5: organization policy slot (D-7): no policy is defined yet.
    // 6
    if r.privacy_denied {
        return drop(out, "6", "PRIVACY");
    }
    let mut decisions: Vec<(Channel, ChannelDecision)> = input
        .channels
        .iter()
        .map(|c| (*c, ChannelDecision::Deliver))
        .collect();
    let set = |ch: Channel, d: ChannelDecision, decisions: &mut Vec<(Channel, ChannelDecision)>| {
        for (c, slot) in decisions.iter_mut() {
            if *c == ch && *slot == ChannelDecision::Deliver {
                *slot = d.clone();
            }
        }
    };
    // 7
    if !input.prefs.category_enabled(category) {
        out.rule_trace.push("7");
        set(
            Channel::Push,
            ChannelDecision::Suppress {
                reason: "CATEGORY_OFF",
            },
            &mut decisions,
        );
        set(
            Channel::Email,
            ChannelDecision::Suppress {
                reason: "CATEGORY_OFF",
            },
            &mut decisions,
        );
        out.silent_inbox = true;
    }
    // 8
    for ch in [Channel::Push, Channel::Email, Channel::InApp] {
        if !input.prefs.channel_enabled(category, ch) {
            out.rule_trace.push("8");
            set(
                ch,
                ChannelDecision::Suppress {
                    reason: "CHANNEL_OFF",
                },
                &mut decisions,
            );
        }
    }
    // 9
    let level = input.mute_level.as_deref().unwrap_or("ALL");
    let effective = if level == "MENTIONS" && (input.private_e2ee || !input.mentions_recipient) {
        "MUTED"
    } else {
        level
    };
    if effective == "MUTED" {
        out.rule_trace.push("9");
        set(
            Channel::Push,
            ChannelDecision::Suppress { reason: "MUTED" },
            &mut decisions,
        );
        out.silent_inbox = true;
    }
    // 10
    if let Some(until) = input.quiet_window_end {
        out.rule_trace.push("10");
        set(
            Channel::Push,
            ChannelDecision::Defer { until },
            &mut decisions,
        );
    }
    // 11
    if !input.has_push_endpoint {
        out.rule_trace.push("11");
        for (c, slot) in decisions.iter_mut() {
            if *c == Channel::Push
                && matches!(
                    slot,
                    ChannelDecision::Deliver | ChannelDecision::Defer { .. }
                )
            {
                *slot = ChannelDecision::Suppress {
                    reason: "NO_PUSH_ENDPOINT",
                };
            }
        }
    }
    // 12
    if input.pushes_last_hour >= PUSH_CAP_PER_HOUR {
        out.rule_trace.push("12");
        set(
            Channel::Push,
            ChannelDecision::Suppress { reason: "PUSH_CAP" },
            &mut decisions,
        );
    }
    out.channels = decisions
        .into_iter()
        .map(|(c, d)| (c.as_str().to_string(), d))
        .collect();
    out
}

#[cfg(test)]
mod tests {
    //! Precedence cases 1–12 (roadmap N3 tests).
    use super::*;
    use crate::preferences::ChannelSwitches;

    fn base(category: PrefCategory, channels: &[Channel]) -> PolicyInput {
        PolicyInput {
            category: Some(category),
            channels: channels.to_vec(),
            recipient_authorized: true,
            has_push_endpoint: true,
            ..Default::default()
        }
    }

    fn d(o: &PolicyOutcome, c: Channel) -> ChannelDecision {
        o.decision(c).cloned().unwrap()
    }

    #[test]
    fn case_1_unauthorized_recipients_get_nothing_even_security() {
        let mut i = base(PrefCategory::Security, &[Channel::Email]);
        i.recipient_authorized = false;
        assert_eq!(evaluate(&i).dropped, Some("RECIPIENT_NOT_AUTHORIZED"));
    }

    #[test]
    fn case_2_operator_hold_drops_all_but_security() {
        let mut i = base(
            PrefCategory::ContactRequests,
            &[Channel::InApp, Channel::Push],
        );
        i.restriction = Some(Restriction {
            operator_hold: true,
            ..Default::default()
        });
        assert_eq!(evaluate(&i).dropped, Some("OPERATOR_HOLD"));
        let mut s = base(PrefCategory::Security, &[Channel::Email, Channel::InApp]);
        s.restriction = i.restriction;
        assert_eq!(evaluate(&s).dropped, None);
    }

    #[test]
    fn case_3_security_ignores_preferences_mute_and_quiet_hours() {
        let mut i = base(
            PrefCategory::Security,
            &[Channel::Email, Channel::InApp, Channel::Push],
        );
        i.prefs.categories.insert("directMessages", false);
        i.mute_level = Some("MUTED".into());
        i.quiet_window_end = Some(Utc::now());
        let o = evaluate(&i);
        assert_eq!(d(&o, Channel::Email), ChannelDecision::Deliver);
        assert_eq!(d(&o, Channel::InApp), ChannelDecision::Deliver);
        assert!(o.decision(Channel::Push).is_none());
        assert!(!o.silent_inbox);
    }

    #[test]
    fn case_4_blocks_drop_social_categories_only() {
        let mut i = base(PrefCategory::DirectMessages, &[Channel::Push]);
        i.restriction = Some(Restriction {
            blocked: true,
            ..Default::default()
        });
        assert_eq!(evaluate(&i).dropped, Some("BLOCKED"));
        let mut o = base(PrefCategory::OrganizationUpdates, &[Channel::InApp]);
        o.restriction = i.restriction;
        assert_eq!(evaluate(&o).dropped, None);
    }

    #[test]
    fn case_6_privacy_drops() {
        let mut i = base(PrefCategory::ContactRequests, &[Channel::InApp]);
        i.restriction = Some(Restriction {
            privacy_denied: true,
            ..Default::default()
        });
        assert_eq!(evaluate(&i).dropped, Some("PRIVACY"));
    }

    #[test]
    fn case_7_category_off_keeps_a_silent_inbox_item() {
        let mut i = base(
            PrefCategory::MeetingReminders,
            &[Channel::Email, Channel::InApp, Channel::Push],
        );
        i.prefs.categories.insert("meetingReminders", false);
        let o = evaluate(&i);
        assert_eq!(
            d(&o, Channel::Email),
            ChannelDecision::Suppress {
                reason: "CATEGORY_OFF"
            }
        );
        assert_eq!(
            d(&o, Channel::Push),
            ChannelDecision::Suppress {
                reason: "CATEGORY_OFF"
            }
        );
        assert_eq!(d(&o, Channel::InApp), ChannelDecision::Deliver);
        assert!(o.silent_inbox);
    }

    #[test]
    fn case_8_channel_off_removes_only_that_channel() {
        let mut i = base(
            PrefCategory::ContactRequests,
            &[Channel::InApp, Channel::Push],
        );
        i.prefs.channels.insert(
            "contactRequests".into(),
            ChannelSwitches {
                push: Some(false),
                ..Default::default()
            },
        );
        let o = evaluate(&i);
        assert_eq!(
            d(&o, Channel::Push),
            ChannelDecision::Suppress {
                reason: "CHANNEL_OFF"
            }
        );
        assert_eq!(d(&o, Channel::InApp), ChannelDecision::Deliver);
    }

    #[test]
    fn case_9_mute_and_mentions() {
        let mut i = base(PrefCategory::GroupMessages, &[Channel::Push]);
        i.mute_level = Some("MENTIONS".into());
        assert_eq!(
            d(&evaluate(&i), Channel::Push),
            ChannelDecision::Suppress { reason: "MUTED" }
        );
        i.mentions_recipient = true;
        assert_eq!(d(&evaluate(&i), Channel::Push), ChannelDecision::Deliver);
        i.private_e2ee = true;
        assert_eq!(
            d(&evaluate(&i), Channel::Push),
            ChannelDecision::Suppress { reason: "MUTED" }
        );
    }

    #[test]
    fn case_10_quiet_hours_defer_push_never_email() {
        let until = Utc::now() + chrono::Duration::hours(3);
        let mut i = base(
            PrefCategory::MeetingInvitations,
            &[Channel::Push, Channel::Email],
        );
        i.quiet_window_end = Some(until);
        let o = evaluate(&i);
        assert_eq!(d(&o, Channel::Push), ChannelDecision::Defer { until });
        assert_eq!(d(&o, Channel::Email), ChannelDecision::Deliver);
    }

    #[test]
    fn case_11_and_12_endpoints_and_caps() {
        let mut i = base(PrefCategory::MissedCalls, &[Channel::Push, Channel::InApp]);
        i.has_push_endpoint = false;
        assert_eq!(
            d(&evaluate(&i), Channel::Push),
            ChannelDecision::Suppress {
                reason: "NO_PUSH_ENDPOINT"
            }
        );
        i.has_push_endpoint = true;
        i.pushes_last_hour = PUSH_CAP_PER_HOUR;
        assert_eq!(
            d(&evaluate(&i), Channel::Push),
            ChannelDecision::Suppress { reason: "PUSH_CAP" }
        );
        assert_eq!(d(&evaluate(&i), Channel::InApp), ChannelDecision::Deliver);
    }

    #[test]
    fn the_snapshot_serializes_without_personal_data() {
        let o = evaluate(&base(PrefCategory::Voicemail, &[Channel::InApp]));
        let s = serde_json::to_string(&o).unwrap();
        assert!(s.contains("\"IN_APP\""));
        assert!(s.contains("ruleTrace"));
    }
}
