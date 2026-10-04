//! MeetingPermissionService — built-in roles, the versioned baseline permission matrix
//! (spec v1.1 Appendix A), controlled policy overrides, role-grant authority and
//! media-permission projection.
//!
//! Handlers never compare roles; they ask this module.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::domain::{MediaCapturePolicy, Role};

/// v2 adds the Part F collaboration permissions; v1 cells are unchanged.
pub const BASELINE_VERSION: &str = "baseline-v2";

/// Semantic conference permissions (stable identifiers for later parts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    ConferenceStart,
    ConferenceEnd,
    ConferenceLock,
    ConferenceConfigureLive,
    ParticipantAdmit,
    ParticipantDeny,
    ParticipantReturnToWaiting,
    ParticipantRemove,
    ParticipantBan,
    ParticipantMute,
    ParticipantVideoDisable,
    ParticipantRoleChange,
    CohostAssign,
    AudioPublish,
    VideoPublish,
    ScreensharePublish,
    ScreenshareManage,
    MediaSubscribe,
    ChatSend,
    ChatModerate,
    PollManage,
    QnaManage,
    BreakoutManage,
    RecordingStart,
    RecordingStop,
    TranscriptionStart,
    ArtifactRead,
    // Part F (baseline-v2)
    ParticipantAudioLock,
    ParticipantVideoLock,
    ParticipantScreenShareStop,
    ParticipantSpotlight,
    ChatRead,
    ChatPrivate,
    ReactionSend,
    HandRaise,
    HandLowerSelf,
    HandLowerOther,
    BreakoutCreate,
    BreakoutAssign,
    BreakoutMove,
    BreakoutEnterAny,
    BreakoutAnnounce,
    BreakoutClose,
    PollCreate,
    PollOpen,
    PollClose,
    PollRespond,
    PollViewResults,
    QnaSubmit,
    QnaVote,
    QnaAnswer,
    QnaDismiss,
    QnaPin,
}

impl Permission {
    pub const ALL: &'static [Permission] = &[
        Permission::ConferenceStart,
        Permission::ConferenceEnd,
        Permission::ConferenceLock,
        Permission::ConferenceConfigureLive,
        Permission::ParticipantAdmit,
        Permission::ParticipantDeny,
        Permission::ParticipantReturnToWaiting,
        Permission::ParticipantRemove,
        Permission::ParticipantBan,
        Permission::ParticipantMute,
        Permission::ParticipantVideoDisable,
        Permission::ParticipantRoleChange,
        Permission::CohostAssign,
        Permission::AudioPublish,
        Permission::VideoPublish,
        Permission::ScreensharePublish,
        Permission::ScreenshareManage,
        Permission::MediaSubscribe,
        Permission::ChatSend,
        Permission::ChatModerate,
        Permission::PollManage,
        Permission::QnaManage,
        Permission::BreakoutManage,
        Permission::RecordingStart,
        Permission::RecordingStop,
        Permission::TranscriptionStart,
        Permission::ArtifactRead,
        Permission::ParticipantAudioLock,
        Permission::ParticipantVideoLock,
        Permission::ParticipantScreenShareStop,
        Permission::ParticipantSpotlight,
        Permission::ChatRead,
        Permission::ChatPrivate,
        Permission::ReactionSend,
        Permission::HandRaise,
        Permission::HandLowerSelf,
        Permission::HandLowerOther,
        Permission::BreakoutCreate,
        Permission::BreakoutAssign,
        Permission::BreakoutMove,
        Permission::BreakoutEnterAny,
        Permission::BreakoutAnnounce,
        Permission::BreakoutClose,
        Permission::PollCreate,
        Permission::PollOpen,
        Permission::PollClose,
        Permission::PollRespond,
        Permission::PollViewResults,
        Permission::QnaSubmit,
        Permission::QnaVote,
        Permission::QnaAnswer,
        Permission::QnaDismiss,
        Permission::QnaPin,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Permission::ConferenceStart => "conference.start",
            Permission::ConferenceEnd => "conference.end",
            Permission::ConferenceLock => "conference.lock",
            Permission::ConferenceConfigureLive => "conference.configure_live",
            Permission::ParticipantAdmit => "participant.admit",
            Permission::ParticipantDeny => "participant.deny",
            Permission::ParticipantReturnToWaiting => "participant.return_to_waiting",
            Permission::ParticipantRemove => "participant.remove",
            Permission::ParticipantBan => "participant.ban",
            Permission::ParticipantMute => "participant.mute",
            Permission::ParticipantVideoDisable => "participant.video_disable",
            Permission::ParticipantRoleChange => "participant.role_change",
            Permission::CohostAssign => "cohost.assign",
            Permission::AudioPublish => "audio.publish",
            Permission::VideoPublish => "video.publish",
            Permission::ScreensharePublish => "screenshare.publish",
            Permission::ScreenshareManage => "screenshare.manage",
            Permission::MediaSubscribe => "media.subscribe",
            Permission::ChatSend => "chat.send",
            Permission::ChatModerate => "chat.moderate",
            Permission::PollManage => "poll.manage",
            Permission::QnaManage => "qna.manage",
            Permission::BreakoutManage => "breakout.manage",
            Permission::RecordingStart => "recording.start",
            Permission::RecordingStop => "recording.stop",
            Permission::TranscriptionStart => "transcription.start",
            Permission::ArtifactRead => "artifact.read",
            Permission::ParticipantAudioLock => "participant.audio_lock",
            Permission::ParticipantVideoLock => "participant.video_lock",
            Permission::ParticipantScreenShareStop => "participant.screen_share_stop",
            Permission::ParticipantSpotlight => "participant.spotlight",
            Permission::ChatRead => "chat.read",
            Permission::ChatPrivate => "chat.private",
            Permission::ReactionSend => "reaction.send",
            Permission::HandRaise => "hand.raise",
            Permission::HandLowerSelf => "hand.lower_self",
            Permission::HandLowerOther => "hand.lower_other",
            Permission::BreakoutCreate => "breakout.create",
            Permission::BreakoutAssign => "breakout.assign",
            Permission::BreakoutMove => "breakout.move",
            Permission::BreakoutEnterAny => "breakout.enter_any",
            Permission::BreakoutAnnounce => "breakout.announce",
            Permission::BreakoutClose => "breakout.close",
            Permission::PollCreate => "poll.create",
            Permission::PollOpen => "poll.open",
            Permission::PollClose => "poll.close",
            Permission::PollRespond => "poll.respond",
            Permission::PollViewResults => "poll.view_results",
            Permission::QnaSubmit => "qna.submit",
            Permission::QnaVote => "qna.vote",
            Permission::QnaAnswer => "qna.answer",
            Permission::QnaDismiss => "qna.dismiss",
            Permission::QnaPin => "qna.pin",
        }
    }

    pub fn from_key(key: &str) -> Option<Permission> {
        Permission::ALL.iter().copied().find(|p| p.key() == key)
    }
}

/// One cell of the baseline matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Yes,
    No,
    /// Policy-controlled; the boolean is the baseline default.
    Policy(bool),
}

use Cell::{No as N, Policy as P, Yes as Y};

/// Baseline matrix (spec v1.1 Appendix A). POLICY defaults are conservative for
/// destructive/escalating operations.
pub fn baseline(role: Role, permission: Permission) -> Cell {
    use Permission::*;
    // Appendix A rows → semantic permissions.
    let row = match permission {
        ConferenceStart => 0,
        ConferenceEnd => 1,
        ConferenceLock => 2,
        ParticipantAdmit | ParticipantDeny | ParticipantReturnToWaiting => 3,
        ParticipantRemove => 4,
        ParticipantBan => 5,
        CohostAssign => 6,
        ParticipantRoleChange => 7,
        ParticipantMute
        | ParticipantVideoDisable
        | ScreenshareManage
        | ConferenceConfigureLive
        | ParticipantAudioLock
        | ParticipantVideoLock
        | ParticipantScreenShareStop
        | ParticipantSpotlight
        | HandLowerOther => 8,
        ScreensharePublish => 9,
        BreakoutManage | BreakoutCreate | BreakoutAssign | BreakoutMove | BreakoutEnterAny
        | BreakoutAnnounce | BreakoutClose => 10,
        PollManage | QnaManage | ChatModerate | PollCreate | PollOpen | PollClose
        | PollViewResults | QnaAnswer | QnaDismiss | QnaPin => 11,
        RecordingStart | RecordingStop | TranscriptionStart => 12,
        AudioPublish | VideoPublish => 13,
        MediaSubscribe => 14,
        // Attendee collaboration: everyone by default; VIEWER is POLICY so view-only
        // never silently means collaboration-disabled (Part F §14), and the meeting
        // policy can still narrow it.
        ChatSend | ChatRead | ChatPrivate | ReactionSend | HandRaise | PollRespond | QnaSubmit
        | QnaVote => {
            return match role {
                Role::Viewer => P(true),
                _ => Y,
            }
        }
        // Lowering one's own hand is always allowed.
        HandLowerSelf => return Y,
        ArtifactRead => {
            return match role {
                Role::Owner | Role::Host => Y,
                Role::CoHost => P(true),
                _ => P(false),
            }
        }
    };
    //                 start end    lock     admit   remove ban      cohost   rolechg  avlocks  share    brkout   polls    record   publish subscr
    const OWNER: [Cell; 15] = [Y, Y, Y, Y, Y, Y, Y, Y, Y, Y, Y, Y, P(true), Y, Y];
    const HOST: [Cell; 15] = [Y, Y, Y, Y, Y, Y, Y, Y, Y, Y, Y, Y, P(true), Y, Y];
    const CO_HOST: [Cell; 15] = [
        P(true),
        P(false),
        Y,
        Y,
        Y,
        P(false),
        P(false),
        Y,
        Y,
        Y,
        Y,
        Y,
        P(false),
        Y,
        Y,
    ];
    const MODERATOR: [Cell; 15] = [
        N,
        N,
        P(false),
        P(true),
        Y,
        P(false),
        N,
        P(false),
        P(true),
        P(true),
        P(false),
        Y,
        N,
        Y,
        Y,
    ];
    const PRESENTER: [Cell; 15] = [N, N, N, N, N, N, N, N, N, Y, N, P(false), N, Y, Y];
    const PARTICIPANT: [Cell; 15] = [N, N, N, N, N, N, N, N, N, P(true), N, N, N, Y, Y];
    const VIEWER: [Cell; 15] = [N, N, N, N, N, N, N, N, N, N, N, N, N, N, Y];
    let table = match role {
        Role::Owner => &OWNER,
        Role::Host => &HOST,
        Role::CoHost => &CO_HOST,
        Role::Moderator => &MODERATOR,
        Role::Presenter => &PRESENTER,
        Role::Participant => &PARTICIPANT,
        Role::Viewer => &VIEWER,
    };
    table[row]
}

/// Controlled per-space / organization overrides of POLICY cells only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RolePolicy {
    /// role → permission key → allowed. Only POLICY cells may be overridden.
    #[serde(default)]
    pub overrides: HashMap<Role, HashMap<String, bool>>,
}

impl RolePolicy {
    pub fn from_json(value: &serde_json::Value) -> RolePolicy {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    /// Rejects overrides of fixed Y/N cells and unknown permission keys.
    pub fn validate(&self) -> Result<(), String> {
        for (role, perms) in &self.overrides {
            for key in perms.keys() {
                let perm = Permission::from_key(key)
                    .ok_or_else(|| format!("unknown permission '{key}'"))?;
                if !matches!(baseline(*role, perm), Cell::Policy(_)) {
                    return Err(format!(
                        "permission '{key}' is not policy-controlled for role {role}"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Everything needed to evaluate a permission for a conference/space.
#[derive(Debug, Clone)]
pub struct PermissionContext {
    pub role_policy: RolePolicy,
    pub recording_policy: MediaCapturePolicy,
    pub transcription_policy: MediaCapturePolicy,
}

impl Default for PermissionContext {
    fn default() -> Self {
        Self {
            role_policy: RolePolicy::default(),
            recording_policy: MediaCapturePolicy::HostOptional,
            transcription_policy: MediaCapturePolicy::Disabled,
        }
    }
}

impl PermissionContext {
    pub fn for_space(space: &crate::models::meeting_spaces::Model) -> Self {
        Self {
            role_policy: RolePolicy::from_json(&space.role_policy),
            recording_policy: space.recording_policy,
            transcription_policy: space.transcription_policy,
        }
    }

    pub fn allows(&self, role: Role, permission: Permission) -> bool {
        let base = match baseline(role, permission) {
            Cell::Yes => true,
            Cell::No => false,
            Cell::Policy(default) => self
                .role_policy
                .overrides
                .get(&role)
                .and_then(|m| m.get(permission.key()))
                .copied()
                .unwrap_or(default),
        };
        let capture_allowed = match permission {
            Permission::RecordingStart | Permission::RecordingStop => {
                self.recording_policy != MediaCapturePolicy::Disabled
            }
            Permission::TranscriptionStart => {
                self.transcription_policy != MediaCapturePolicy::Disabled
            }
            _ => true,
        };
        base && capture_allowed
    }

    pub fn permission_keys(&self, role: Role) -> Vec<&'static str> {
        Permission::ALL
            .iter()
            .filter(|p| self.allows(role, **p))
            .map(|p| p.key())
            .collect()
    }

    pub fn media(&self, role: Role) -> MediaPermissions {
        MediaPermissions {
            audio_publish: self.allows(role, Permission::AudioPublish),
            video_publish: self.allows(role, Permission::VideoPublish),
            screen_share_publish: self.allows(role, Permission::ScreensharePublish),
            data_publish: self.allows(role, Permission::ChatSend),
            subscribe: self.allows(role, Permission::MediaSubscribe),
            moderate_media: self.allows(role, Permission::ParticipantMute),
            record: self.allows(role, Permission::RecordingStart),
        }
    }
}

/// Media permission projection handed to the RTC provider (Part E enforces it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaPermissions {
    pub audio_publish: bool,
    pub video_publish: bool,
    pub screen_share_publish: bool,
    pub data_publish: bool,
    pub subscribe: bool,
    pub moderate_media: bool,
    pub record: bool,
}

impl MediaPermissions {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn can_publish_anything(&self) -> bool {
        self.audio_publish || self.video_publish || self.screen_share_publish
    }
}

/// Space-level (persistent definition) permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpacePermission {
    View,
    Configure,
    ManageMembers,
    ViewMembers,
    Invite,
    ScheduleConferences,
    Archive,
    TransferOwnership,
    Duplicate,
}

pub fn space_allows(role: Role, permission: SpacePermission, ctx: &PermissionContext) -> bool {
    match permission {
        SpacePermission::View => true,
        SpacePermission::Configure
        | SpacePermission::ManageMembers
        | SpacePermission::Duplicate => {
            matches!(role, Role::Owner | Role::Host)
        }
        SpacePermission::ViewMembers => matches!(role, Role::Owner | Role::Host | Role::CoHost),
        SpacePermission::Invite => ctx.allows(role, Permission::ParticipantAdmit),
        SpacePermission::ScheduleConferences => ctx.allows(role, Permission::ConferenceStart),
        SpacePermission::Archive | SpacePermission::TransferOwnership => role == Role::Owner,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantDenied {
    OwnerRequiresTransfer,
    CannotModifyOwner,
    CannotModifySelf,
    MissingRoleChangePermission,
    TargetNotBelowActor,
    RoleAboveGrantAuthority,
    MissingCohostAssign,
}

impl GrantDenied {
    pub fn message(&self) -> &'static str {
        match self {
            GrantDenied::OwnerRequiresTransfer => {
                "Ownership can only change through owner transfer"
            }
            GrantDenied::CannotModifyOwner => "The owner's role cannot be changed",
            GrantDenied::CannotModifySelf => "You cannot change your own role",
            GrantDenied::MissingRoleChangePermission => "You are not permitted to change roles",
            GrantDenied::TargetNotBelowActor => {
                "You can only change roles of less privileged participants"
            }
            GrantDenied::RoleAboveGrantAuthority => "You cannot grant a role at or above your own",
            GrantDenied::MissingCohostAssign => "You are not permitted to assign co-hosts",
        }
    }
}

/// Whether `actor` may grant `new_role` (to a membership, invitation or participant).
pub fn check_grant(
    actor: Role,
    new_role: Role,
    ctx: &PermissionContext,
) -> Result<(), GrantDenied> {
    if new_role == Role::Owner {
        return Err(GrantDenied::OwnerRequiresTransfer);
    }
    if new_role == Role::CoHost && !ctx.allows(actor, Permission::CohostAssign) {
        return Err(GrantDenied::MissingCohostAssign);
    }
    let cohost_by_delegate = new_role == Role::CoHost && actor.rank() >= Role::CoHost.rank();
    if new_role.rank() >= actor.rank() && !cohost_by_delegate {
        return Err(GrantDenied::RoleAboveGrantAuthority);
    }
    Ok(())
}

/// Full participant role-change authority check.
pub fn check_role_change(
    actor: Role,
    target_current: Role,
    new_role: Role,
    is_self: bool,
    ctx: &PermissionContext,
) -> Result<(), GrantDenied> {
    if new_role == Role::Owner {
        return Err(GrantDenied::OwnerRequiresTransfer);
    }
    if target_current == Role::Owner {
        return Err(GrantDenied::CannotModifyOwner);
    }
    if is_self {
        return Err(GrantDenied::CannotModifySelf);
    }
    if !ctx.allows(actor, Permission::ParticipantRoleChange) {
        return Err(GrantDenied::MissingRoleChangePermission);
    }
    if target_current.rank() >= actor.rank() {
        return Err(GrantDenied::TargetNotBelowActor);
    }
    check_grant(actor, new_role, ctx)
}

/// Moderation (remove/ban/return-to-waiting) may only target strictly less privileged roles.
pub fn can_moderate(actor: Role, target: Role) -> bool {
    target != Role::Owner && target.rank() < actor.rank()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> PermissionContext {
        PermissionContext::default()
    }

    #[test]
    fn appendix_a_fixed_cells() {
        let c = ctx();
        for role in [Role::Owner, Role::Host] {
            for p in [
                Permission::ConferenceStart,
                Permission::ConferenceEnd,
                Permission::ConferenceLock,
                Permission::ParticipantAdmit,
                Permission::ParticipantRemove,
                Permission::ParticipantBan,
                Permission::CohostAssign,
                Permission::ParticipantRoleChange,
                Permission::ScreensharePublish,
                Permission::BreakoutManage,
                Permission::PollManage,
                Permission::AudioPublish,
                Permission::MediaSubscribe,
            ] {
                assert!(c.allows(role, p), "{role} {p:?}");
            }
        }
        assert!(c.allows(Role::CoHost, Permission::ConferenceLock));
        assert!(c.allows(Role::CoHost, Permission::ParticipantAdmit));
        assert!(c.allows(Role::CoHost, Permission::ParticipantRoleChange));
        assert!(!c.allows(Role::Moderator, Permission::ConferenceStart));
        assert!(!c.allows(Role::Moderator, Permission::CohostAssign));
        assert!(!c.allows(Role::Moderator, Permission::RecordingStart));
        assert!(c.allows(Role::Moderator, Permission::ParticipantRemove));
        assert!(c.allows(Role::Moderator, Permission::PollManage));
        assert!(c.allows(Role::Presenter, Permission::ScreensharePublish));
        assert!(!c.allows(Role::Presenter, Permission::ParticipantAdmit));
        assert!(c.allows(Role::Participant, Permission::AudioPublish));
        assert!(!c.allows(Role::Participant, Permission::ParticipantAdmit));
        // Viewer: subscribe + attendee collaboration only (Part F §14), never media
        // publication or any management permission.
        let viewer_allowed = [
            Permission::MediaSubscribe,
            Permission::ChatSend,
            Permission::ChatRead,
            Permission::ChatPrivate,
            Permission::ReactionSend,
            Permission::HandRaise,
            Permission::HandLowerSelf,
            Permission::PollRespond,
            Permission::QnaSubmit,
            Permission::QnaVote,
        ];
        for p in Permission::ALL {
            if viewer_allowed.contains(p) {
                assert!(c.allows(Role::Viewer, *p), "viewer should have {p:?}");
                continue;
            }
            assert!(!c.allows(Role::Viewer, *p), "viewer must not have {p:?}");
        }
        assert!(c.allows(Role::Viewer, Permission::MediaSubscribe));
    }

    #[test]
    fn part_f_permissions_follow_appendix_rows() {
        let c = ctx();
        for role in [Role::Owner, Role::Host, Role::CoHost] {
            for p in [
                Permission::ParticipantAudioLock,
                Permission::ParticipantVideoLock,
                Permission::ParticipantScreenShareStop,
                Permission::ParticipantSpotlight,
                Permission::HandLowerOther,
                Permission::BreakoutCreate,
                Permission::BreakoutMove,
                Permission::BreakoutEnterAny,
                Permission::BreakoutClose,
                Permission::PollCreate,
                Permission::PollViewResults,
                Permission::QnaAnswer,
                Permission::QnaPin,
            ] {
                assert!(c.allows(role, p), "{role} {p:?}");
            }
        }
        // Moderator: media controls are POLICY(default allow), breakouts POLICY(deny).
        assert!(c.allows(Role::Moderator, Permission::ParticipantAudioLock));
        assert!(!c.allows(Role::Moderator, Permission::BreakoutCreate));
        assert!(c.allows(Role::Moderator, Permission::QnaAnswer));
        for role in [Role::Presenter, Role::Participant, Role::Viewer] {
            assert!(!c.allows(role, Permission::ParticipantAudioLock), "{role}");
            assert!(!c.allows(role, Permission::BreakoutMove), "{role}");
            assert!(!c.allows(role, Permission::PollViewResults) || role == Role::Presenter);
            assert!(c.allows(role, Permission::HandRaise));
            assert!(c.allows(role, Permission::QnaSubmit));
            assert!(c.allows(role, Permission::PollRespond));
        }
        // Policy can take attendee collaboration away from viewers only.
        let mut policy = RolePolicy::default();
        policy
            .overrides
            .entry(Role::Viewer)
            .or_default()
            .insert("chat.send".into(), false);
        assert!(policy.validate().is_ok());
        let narrowed = PermissionContext {
            role_policy: policy,
            ..ctx()
        };
        assert!(!narrowed.allows(Role::Viewer, Permission::ChatSend));
        assert_eq!(BASELINE_VERSION, "baseline-v2");
    }

    #[test]
    fn viewer_never_receives_publish_grants() {
        let m = ctx().media(Role::Viewer);
        assert!(!m.can_publish_anything());
        assert!(m.subscribe);
    }

    #[test]
    fn policy_overrides_only_apply_to_policy_cells() {
        let mut policy = RolePolicy::default();
        policy
            .overrides
            .entry(Role::CoHost)
            .or_default()
            .insert("conference.end".into(), true);
        assert!(policy.validate().is_ok());
        let c = PermissionContext {
            role_policy: policy,
            ..ctx()
        };
        assert!(c.allows(Role::CoHost, Permission::ConferenceEnd));

        let mut bad = RolePolicy::default();
        bad.overrides
            .entry(Role::Viewer)
            .or_default()
            .insert("audio.publish".into(), true);
        assert!(bad.validate().is_err());
        let mut unknown = RolePolicy::default();
        unknown
            .overrides
            .entry(Role::Host)
            .or_default()
            .insert("god.mode".into(), true);
        assert!(unknown.validate().is_err());
    }

    #[test]
    fn recording_respects_space_policy() {
        let mut c = ctx();
        assert!(c.allows(Role::Host, Permission::RecordingStart));
        c.recording_policy = MediaCapturePolicy::Disabled;
        assert!(!c.allows(Role::Host, Permission::RecordingStart));
        assert!(!c.allows(Role::Owner, Permission::TranscriptionStart));
    }

    #[test]
    fn grant_authority_rules() {
        let c = ctx();
        assert_eq!(
            check_grant(Role::Host, Role::Owner, &c),
            Err(GrantDenied::OwnerRequiresTransfer)
        );
        assert!(check_grant(Role::Owner, Role::Host, &c).is_ok());
        assert_eq!(
            check_grant(Role::Host, Role::Host, &c),
            Err(GrantDenied::RoleAboveGrantAuthority)
        );
        assert!(check_grant(Role::Host, Role::CoHost, &c).is_ok());
        assert_eq!(
            check_grant(Role::CoHost, Role::CoHost, &c),
            Err(GrantDenied::MissingCohostAssign)
        );
        assert!(check_grant(Role::CoHost, Role::Moderator, &c).is_ok());
        assert_eq!(
            check_grant(Role::Moderator, Role::CoHost, &c),
            Err(GrantDenied::MissingCohostAssign)
        );
        assert_eq!(
            check_grant(Role::Participant, Role::Presenter, &c),
            Err(GrantDenied::RoleAboveGrantAuthority)
        );
    }

    #[test]
    fn role_change_rules() {
        let c = ctx();
        // self-promotion
        assert_eq!(
            check_role_change(Role::Participant, Role::Participant, Role::Host, true, &c),
            Err(GrantDenied::CannotModifySelf)
        );
        assert_eq!(
            check_role_change(Role::Participant, Role::Participant, Role::CoHost, true, &c),
            Err(GrantDenied::CannotModifySelf)
        );
        // moderator escalation (role change is POLICY default deny)
        assert_eq!(
            check_role_change(Role::Moderator, Role::Participant, Role::Owner, false, &c),
            Err(GrantDenied::OwnerRequiresTransfer)
        );
        assert_eq!(
            check_role_change(
                Role::Moderator,
                Role::Participant,
                Role::Presenter,
                false,
                &c
            ),
            Err(GrantDenied::MissingRoleChangePermission)
        );
        // co-host cannot touch host
        assert_eq!(
            check_role_change(Role::CoHost, Role::Host, Role::Participant, false, &c),
            Err(GrantDenied::TargetNotBelowActor)
        );
        assert!(check_role_change(Role::Host, Role::Participant, Role::CoHost, false, &c).is_ok());
        assert!(
            check_role_change(Role::CoHost, Role::Participant, Role::Presenter, false, &c).is_ok()
        );
        assert_eq!(
            check_role_change(Role::Host, Role::Owner, Role::Participant, false, &c),
            Err(GrantDenied::CannotModifyOwner)
        );
    }

    #[test]
    fn moderation_targets_must_be_less_privileged() {
        assert!(can_moderate(Role::Host, Role::Participant));
        assert!(!can_moderate(Role::Host, Role::Owner));
        assert!(!can_moderate(Role::CoHost, Role::Host));
        assert!(!can_moderate(Role::Moderator, Role::Moderator));
    }
}
