//! Phase 2 feature flags and meeting-v2 tunables (spec v1.1 §7).
//!
//! Loaded from the environment (`FEATURE_*`). Defaults are rollout-safe: v2 is off
//! unless enabled, and `legacy_meeting_write_disabled` is off unless explicitly set
//! (legacy write retirement is a rollout decision, Parts J–L §38–§39).

use std::collections::HashSet;
use uuid::Uuid;

pub(crate) fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

pub(crate) fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// `MEETING_V2_WRITE_ALLOWLIST`: comma-separated user ids; malformed entries are ignored
/// and an empty / all-malformed list means "no allowlist" (every authenticated user).
pub(crate) fn parse_allowlist(raw: &str) -> Option<HashSet<Uuid>> {
    let ids: HashSet<Uuid> = raw
        .split(',')
        .filter_map(|s| Uuid::parse_str(s.trim()).ok())
        .collect();
    (!ids.is_empty()).then_some(ids)
}

/// All flags default to off (rollout-safe).
#[derive(Debug, Clone, Default)]
pub struct FeatureFlags {
    /// Mounts and enables the `/api/v2` meeting domain.
    pub meeting_v2_enabled: bool,
    /// Enables v2 waiting-room management and non-OFF waiting-room configuration.
    pub waiting_room_v2_enabled: bool,
    /// Optional allowlist of user IDs permitted to perform v2 writes (controlled rollout).
    /// `None` = every authenticated user.
    pub v2_write_allowlist: Option<HashSet<Uuid>>,
    /// Compare legacy and v2 state on legacy reads (logs drift only).
    pub meeting_v2_shadow_read: bool,
    /// Part E: provider-issued RTC credentials in the v2 join response.
    pub rtc_provider_control_v2: bool,
    pub conference_realtime_enabled: bool,
    pub collaboration_v2_enabled: bool,
    pub recording_v2_enabled: bool,
    pub webinar_mode_enabled: bool,
    pub healthcare_profile_enabled: bool,
    /// Part G: transcription / captions (external processing impact → default off).
    pub transcription_v2_enabled: bool,
    /// Part G: intelligence (AI) processing — disabled by default.
    pub intelligence_v2_enabled: bool,
    /// Part I: client quality-sample ingestion.
    pub quality_telemetry_enabled: bool,
    /// Legacy write retirement switch (`FEATURE_LEGACY_MEETING_WRITE_DISABLED`). When on,
    /// every state-changing `/api/v1` meeting / participant / chat / third-party meeting
    /// route answers `410 LEGACY_WRITE_DISABLED`; legacy reads and `/api/v2` keep working.
    pub legacy_meeting_write_disabled: bool,
}

impl FeatureFlags {
    pub fn from_env() -> Self {
        let allowlist = std::env::var("MEETING_V2_WRITE_ALLOWLIST")
            .ok()
            .and_then(|raw| parse_allowlist(&raw));
        let legacy_write_disabled = env_bool("FEATURE_LEGACY_MEETING_WRITE_DISABLED", false);
        if legacy_write_disabled {
            tracing::warn!(
                "FEATURE_LEGACY_MEETING_WRITE_DISABLED is on: legacy /api/v1 meeting writes \
                 answer 410 LEGACY_WRITE_DISABLED (reads and /api/v2 unaffected)"
            );
        }
        Self {
            meeting_v2_enabled: env_bool("FEATURE_MEETING_V2_ENABLED", false),
            waiting_room_v2_enabled: env_bool("FEATURE_WAITING_ROOM_V2_ENABLED", false),
            v2_write_allowlist: allowlist,
            meeting_v2_shadow_read: env_bool("FEATURE_MEETING_V2_SHADOW_READ", false),
            rtc_provider_control_v2: env_bool("FEATURE_RTC_PROVIDER_CONTROL_V2", false),
            conference_realtime_enabled: env_bool("FEATURE_CONFERENCE_REALTIME_ENABLED", false),
            collaboration_v2_enabled: env_bool("FEATURE_COLLABORATION_V2_ENABLED", false),
            recording_v2_enabled: env_bool("FEATURE_RECORDING_V2_ENABLED", false),
            webinar_mode_enabled: env_bool("FEATURE_WEBINAR_MODE_ENABLED", false),
            healthcare_profile_enabled: env_bool("FEATURE_HEALTHCARE_PROFILE_ENABLED", false),
            transcription_v2_enabled: env_bool("FEATURE_TRANSCRIPTION_V2_ENABLED", false),
            intelligence_v2_enabled: env_bool("FEATURE_INTELLIGENCE_V2_ENABLED", false),
            quality_telemetry_enabled: env_bool("FEATURE_QUALITY_TELEMETRY_ENABLED", false),
            legacy_meeting_write_disabled: legacy_write_disabled,
        }
    }

    /// Everything a test or local developer needs for Parts A–D.
    pub fn all_v2_enabled() -> Self {
        Self {
            meeting_v2_enabled: true,
            waiting_room_v2_enabled: true,
            meeting_v2_shadow_read: true,
            healthcare_profile_enabled: true,
            webinar_mode_enabled: true,
            ..Self::default()
        }
    }

    pub fn may_write_v2(&self, user_id: Uuid) -> bool {
        self.v2_write_allowlist
            .as_ref()
            .map(|ids| ids.contains(&user_id))
            .unwrap_or(true)
    }
}

/// Meeting v2 tunables.
#[derive(Debug, Clone)]
pub struct MeetingV2Config {
    /// bcrypt cost for meeting passcodes.
    pub passcode_hash_cost: u32,
    /// Lifetime of a participant session / RTC credential.
    pub session_ttl_secs: i64,
    /// Maximum invitation lifetime.
    pub invitation_max_ttl_hours: i64,
    /// Failed passcode attempts allowed per window, per (space, client key).
    pub passcode_max_attempts: i64,
    pub passcode_window_secs: u64,
    /// Bounded recurrence materialization horizon.
    pub occurrence_horizon_days: i64,
    pub occurrence_max_per_run: usize,
    pub scheduler_poll_secs: u64,
    /// Maximum concurrent active device sessions for one logical participant.
    pub max_sessions_per_participant: u64,
    /// Part E: RTC credential lifetime (bounded; never longer than the session).
    pub rtc_credential_ttl_secs: i64,
    /// Part E: conference reconciler cadence and batch bound.
    pub reconciler_poll_secs: u64,
    pub reconciler_batch: u64,
    /// Part E: grace before a CONNECTED session absent at the provider is closed.
    pub reconciler_presence_grace_secs: i64,
    /// Parts G–I: artefact storage / recording / quality tunables.
    pub artifacts: super::operations::ArtifactConfig,
    pub quality: super::operations::QualityConfig,
}

impl Default for MeetingV2Config {
    fn default() -> Self {
        Self {
            passcode_hash_cost: 12,
            session_ttl_secs: 4 * 60 * 60,
            invitation_max_ttl_hours: 24 * 90,
            passcode_max_attempts: 10,
            passcode_window_secs: 15 * 60,
            occurrence_horizon_days: 30,
            occurrence_max_per_run: 50,
            scheduler_poll_secs: 30,
            max_sessions_per_participant: 5,
            rtc_credential_ttl_secs: 10 * 60,
            reconciler_poll_secs: 30,
            reconciler_batch: 50,
            reconciler_presence_grace_secs: 30,
            artifacts: super::operations::ArtifactConfig::default(),
            quality: super::operations::QualityConfig::default(),
        }
    }
}

impl MeetingV2Config {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            passcode_hash_cost: env_num("MEETING_PASSCODE_HASH_COST", d.passcode_hash_cost)
                .clamp(4, 31),
            session_ttl_secs: env_num("MEETING_SESSION_TTL_SECS", d.session_ttl_secs),
            invitation_max_ttl_hours: env_num(
                "MEETING_INVITATION_MAX_TTL_HOURS",
                d.invitation_max_ttl_hours,
            ),
            passcode_max_attempts: env_num(
                "MEETING_PASSCODE_MAX_ATTEMPTS",
                d.passcode_max_attempts,
            ),
            passcode_window_secs: env_num("MEETING_PASSCODE_WINDOW_SECS", d.passcode_window_secs),
            occurrence_horizon_days: env_num(
                "MEETING_OCCURRENCE_HORIZON_DAYS",
                d.occurrence_horizon_days,
            ),
            occurrence_max_per_run: env_num(
                "MEETING_OCCURRENCE_MAX_PER_RUN",
                d.occurrence_max_per_run,
            ),
            scheduler_poll_secs: env_num("CONFERENCE_SCHEDULER_POLL_SECS", d.scheduler_poll_secs),
            max_sessions_per_participant: env_num(
                "MEETING_MAX_SESSIONS_PER_PARTICIPANT",
                d.max_sessions_per_participant,
            ),
            rtc_credential_ttl_secs: env_num("RTC_CREDENTIAL_TTL_SECS", d.rtc_credential_ttl_secs)
                .clamp(60, 6 * 60 * 60),
            reconciler_poll_secs: env_num(
                "CONFERENCE_RECONCILER_POLL_SECS",
                d.reconciler_poll_secs,
            )
            .max(1),
            reconciler_batch: env_num("CONFERENCE_RECONCILER_BATCH", d.reconciler_batch)
                .clamp(1, 1000),
            reconciler_presence_grace_secs: env_num(
                "CONFERENCE_RECONCILER_PRESENCE_GRACE_SECS",
                d.reconciler_presence_grace_secs,
            )
            .max(0),
            artifacts: super::operations::ArtifactConfig::from_env(),
            quality: super::operations::QualityConfig::from_env(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_rollout_safe() {
        let f = FeatureFlags::default();
        assert!(!f.meeting_v2_enabled);
        assert!(!f.legacy_meeting_write_disabled);
        assert!(!FeatureFlags::all_v2_enabled().legacy_meeting_write_disabled);
    }

    #[test]
    fn env_values_parse_strictly() {
        // Unique variable names: no interference with other tests.
        for (raw, expected) in [
            ("1", true),
            ("true", true),
            (" YES ", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("enabled", false),
            ("", false),
        ] {
            std::env::set_var("CFMEET_UNIT_FLAG_PARSE", raw);
            assert_eq!(
                env_bool("CFMEET_UNIT_FLAG_PARSE", false),
                expected,
                "{raw:?}"
            );
        }
        std::env::remove_var("CFMEET_UNIT_FLAG_PARSE");
        assert!(env_bool("CFMEET_UNIT_FLAG_PARSE", true), "unset → default");
        assert!(!env_bool("CFMEET_UNIT_FLAG_PARSE", false));
    }

    #[test]
    fn allowlist_parsing_ignores_malformed_entries() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let parsed = parse_allowlist(&format!(" {a}, nope ,{b},")).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&a) && parsed.contains(&b));
        assert!(parse_allowlist("").is_none());
        assert!(
            parse_allowlist("not-a-uuid, also-not").is_none(),
            "no accidental lockout"
        );
    }

    #[test]
    fn every_phase2_flag_defaults_off() {
        let f = FeatureFlags::default();
        assert!(
            !(f.meeting_v2_enabled
                || f.waiting_room_v2_enabled
                || f.meeting_v2_shadow_read
                || f.rtc_provider_control_v2
                || f.conference_realtime_enabled
                || f.collaboration_v2_enabled
                || f.recording_v2_enabled
                || f.webinar_mode_enabled
                || f.healthcare_profile_enabled
                || f.transcription_v2_enabled
                || f.intelligence_v2_enabled
                || f.quality_telemetry_enabled
                || f.legacy_meeting_write_disabled)
        );
        assert!(f.v2_write_allowlist.is_none());
    }

    #[test]
    fn allowlist_restricts_v2_writes() {
        let allowed = Uuid::new_v4();
        let f = FeatureFlags {
            v2_write_allowlist: Some([allowed].into_iter().collect()),
            ..FeatureFlags::default()
        };
        assert!(f.may_write_v2(allowed));
        assert!(!f.may_write_v2(Uuid::new_v4()));
        assert!(FeatureFlags::default().may_write_v2(Uuid::new_v4()));
    }
}
