//! Server-authoritative, provider-neutral media permissions (Part C §22, §23, §25).
//!
//! Every media credential and every provider permission update is derived from three
//! server-side facts only: the participant's persisted authorized permissions
//! (`call_participant_media_permissions`), the Call's kind and the Call's state. A client
//! can never ask for a permission: there is no input type carrying one.
//!
//! * Defaults (policy `APP_MEDIA_V1`): VOICE — publish audio, subscribe; VIDEO — publish
//!   audio and video, subscribe. Data publishing is off by default (least privilege).
//! * A VOICE Call never grants video publishing, whatever is persisted (clamped here and
//!   refused by a database CHECK).
//! * HOLD (§62, ADR 0008): while the Call is HELD — or TRANSFERRING out of a held Call —
//!   nobody publishes audio or video; subscriptions and data stay as authorized, room
//!   membership is preserved. RESUME re-derives the effective permissions from the
//!   persisted authorized ones, so anything revoked in between stays revoked.
//! * Join credentials carry NO media permission at all ([`MediaPermissions::NONE`]): the
//!   server grants the effective permissions to a provider connection only after it has
//!   verified that the connection belongs to a current, authorized session generation.

use serde::Serialize;

use crate::calls::{kinds::CallKind, state::CallState};

/// Version of the application media permission policy.
pub const MEDIA_POLICY_VERSION: &str = "APP_MEDIA_V1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPermissions {
    pub can_publish_audio: bool,
    pub can_publish_video: bool,
    pub can_publish_data: bool,
    pub can_subscribe: bool,
}

impl MediaPermissions {
    /// No media at all: what a join credential grants before server admission, and what a
    /// session holds once its Call ends.
    pub const NONE: MediaPermissions = MediaPermissions {
        can_publish_audio: false,
        can_publish_video: false,
        can_publish_data: false,
        can_subscribe: false,
    };

    /// Policy defaults for a Call kind.
    pub fn default_for(kind: CallKind) -> Self {
        match kind {
            CallKind::Voice => MediaPermissions {
                can_publish_audio: true,
                can_publish_video: false,
                can_publish_data: false,
                can_subscribe: true,
            },
            CallKind::Video => MediaPermissions {
                can_publish_audio: true,
                can_publish_video: true,
                can_publish_data: false,
                can_subscribe: true,
            },
        }
    }

    /// Never more than the Call kind allows.
    pub fn clamp_to(self, kind: CallKind) -> Self {
        MediaPermissions {
            can_publish_video: self.can_publish_video && kind == CallKind::Video,
            ..self
        }
    }

    /// The hold mask: no audio / video publishing; subscription and data unchanged.
    pub fn held(self) -> Self {
        MediaPermissions {
            can_publish_audio: false,
            can_publish_video: false,
            ..self
        }
    }

    /// Effective permissions of a participant whose authorized permissions are
    /// `authorized`, in a Call of `kind` in `state`. `held_before_transfer` tells whether a
    /// TRANSFERRING Call came from HELD.
    pub fn effective(
        authorized: MediaPermissions,
        kind: CallKind,
        state: CallState,
        held_before_transfer: bool,
    ) -> Self {
        let authorized = authorized.clamp_to(kind);
        match state {
            CallState::Held => authorized.held(),
            CallState::Transferring if held_before_transfer => authorized.held(),
            CallState::Dialing
            | CallState::Ringing
            | CallState::Answered
            | CallState::Connected
            | CallState::Transferring => authorized,
            _ => MediaPermissions::NONE,
        }
    }

    pub fn is_none(&self) -> bool {
        *self == MediaPermissions::NONE
    }

    /// Whether `self` grants nothing that `other` does not.
    pub fn within(&self, other: &MediaPermissions) -> bool {
        (!self.can_publish_audio || other.can_publish_audio)
            && (!self.can_publish_video || other.can_publish_video)
            && (!self.can_publish_data || other.can_publish_data)
            && (!self.can_subscribe || other.can_subscribe)
    }
}

/// Call states in which an authorized session may hold a media credential.
pub fn media_state(state: CallState) -> bool {
    matches!(
        state,
        CallState::Dialing
            | CallState::Ringing
            | CallState::Answered
            | CallState::Connected
            | CallState::Held
            | CallState::Transferring
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_never_publishes_video() {
        let voice = MediaPermissions::default_for(CallKind::Voice);
        assert!(voice.can_publish_audio && voice.can_subscribe && !voice.can_publish_video);
        let forced = MediaPermissions {
            can_publish_video: true,
            ..voice
        };
        assert!(!forced.clamp_to(CallKind::Voice).can_publish_video);
        for state in CallState::ALL {
            assert!(
                !MediaPermissions::effective(forced, CallKind::Voice, *state, false)
                    .can_publish_video
            );
        }
        let video = MediaPermissions::default_for(CallKind::Video);
        assert!(video.can_publish_audio && video.can_publish_video && video.can_subscribe);
    }

    #[test]
    fn hold_stops_publishing_and_resume_rederives() {
        let video = MediaPermissions::default_for(CallKind::Video);
        let held = MediaPermissions::effective(video, CallKind::Video, CallState::Held, false);
        assert!(!held.can_publish_audio && !held.can_publish_video && held.can_subscribe);
        let transferring_from_hold =
            MediaPermissions::effective(video, CallKind::Video, CallState::Transferring, true);
        assert_eq!(transferring_from_hold, held);
        // Resume re-derives from the CURRENT authorized permissions.
        let revoked = MediaPermissions {
            can_publish_video: false,
            ..video
        };
        let resumed =
            MediaPermissions::effective(revoked, CallKind::Video, CallState::Connected, false);
        assert!(!resumed.can_publish_video);
    }

    #[test]
    fn terminal_and_pre_routing_states_grant_nothing() {
        let voice = MediaPermissions::default_for(CallKind::Voice);
        for state in [
            CallState::Requested,
            CallState::Authorized,
            CallState::Resolving,
            CallState::Routing,
            CallState::Ending,
            CallState::Ended,
            CallState::Cancelled,
            CallState::Failed,
        ] {
            assert!(MediaPermissions::effective(voice, CallKind::Voice, state, false).is_none());
            assert!(!media_state(state));
        }
    }

    #[test]
    fn within_is_a_partial_order() {
        let voice = MediaPermissions::default_for(CallKind::Voice);
        let video = MediaPermissions::default_for(CallKind::Video);
        assert!(voice.within(&video));
        assert!(!video.within(&voice));
        assert!(MediaPermissions::NONE.within(&voice));
    }
}
