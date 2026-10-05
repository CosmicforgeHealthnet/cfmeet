//! MeetingProfileResolver — specialized conference modes as POLICY PROFILES over the
//! one meeting platform (Phase 2 Part H §38–§39, consumed by Part G).
//!
//! WEBINAR, TOWN_HALL, HEALTHCARE and HIGH_SECURITY are not separate stacks: they are
//! policy defaults selected by meeting type / security profile and refined by explicit
//! overrides. Precedence:
//!
//! ```text
//! base defaults → meeting-type profile → security profile → organization policy
//!               → meeting-space override → conference override
//! ```
//!
//! Restrictions cannot be bypassed by a weaker lower level: the security profile
//! carries hard ceilings and every value the ORGANIZATION sets explicitly becomes a
//! ceiling too. After the space / conference overrides are applied the result is
//! clamped to those ceilings (e.g. a HEALTHCARE space can opt into recording, but
//! never into NOTICE_ONLY consent, public access or external AI).
//!
//! A stored layer that does not validate (unknown keys, wrong types) is treated as a
//! misconfiguration: the resolver returns the restrictive fallback profile instead of
//! guessing (Part H §H-F8).
//!
//! The resolver encodes capabilities, not legal advice: retention periods and
//! external-processing rules are organization configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

use crate::{
    domain::{MeetingType, Role, SecurityProfile},
    models::{conference_sessions, meeting_spaces},
};

/// The specialized mode a space resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MeetingProfile {
    Standard,
    Webinar,
    TownHall,
    Healthcare,
    HighSecurity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConsentMode {
    /// Visible notice only (recording always shows a notice).
    NoticeOnly,
    /// Processing waits for affirmative consent of every affected participant.
    ExplicitConsentRequired,
}

/// What a consent withdrawal does (policy-driven, never one universal rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WithdrawalMode {
    /// Stop future processing that depends on this consent (active explicit-consent
    /// capture of that type stops). Retained artefacts are untouched.
    StopFutureProcessing,
    /// The withdrawing participant must leave (returned to the waiting room).
    RequireParticipantExit,
    /// Stop the whole processing mode for the conference, whatever the consent mode.
    StopProcessingMode,
    /// Record the withdrawal only; nothing stops and nothing is deleted.
    NoRetroactiveDeletion,
}

/// Where processing may run. Ordered from most to least restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProcessingLocation {
    Disabled,
    SelfHostedOnly,
    ExternalAllowed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FieldRequirement {
    Required,
    Optional,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingProfile {
    pub allowed: bool,
    pub consent_mode: ConsentMode,
    pub withdrawal_mode: WithdrawalMode,
    /// Days a READY recording is kept before the retention worker deletes it.
    pub retention_days: u32,
    pub manual_delete_allowed: bool,
    /// Composite layout (`grid` / `speaker`).
    pub layout: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptionProfile {
    pub allowed: bool,
    pub consent_mode: ConsentMode,
    pub withdrawal_mode: WithdrawalMode,
    pub location: ProcessingLocation,
    pub retention_days: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntelligenceProfile {
    pub location: ProcessingLocation,
    pub consent_required: bool,
    pub retention_days: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataSharingProfile {
    pub allowed: bool,
    pub consent_required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportProfile {
    pub chat_export_allowed: bool,
    pub attendance_report_allowed: bool,
    pub poll_report_allowed: bool,
    pub retention_days: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessProfile {
    /// PUBLIC / PUBLIC_PASSCODE entry permitted.
    pub public_access_allowed: bool,
    /// Waiting room applies even when the space says OFF (hosts stay exempt).
    pub waiting_room_forced: bool,
    /// Lifetime of guest / external participant sessions.
    pub guest_session_ttl_secs: u32,
    /// External participants must present an external authorization context.
    pub require_external_context: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditProfile {
    /// Enhanced semantic audit (join, artefact access, consent, admin access).
    pub enhanced: bool,
}

/// Large-audience architecture: a small interactive stage, a subscribe-only audience.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageProfile {
    /// Participants that may publish (every role above VIEWER). 0 = not limited.
    pub max_stage_participants: u32,
    /// PRESENTER-role participants. 0 = not limited.
    pub max_presenters: u32,
    /// Operational viewer capacity (informational; enforced by space capacity).
    pub viewer_capacity: u32,
    pub backstage_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomRegistrationField {
    /// `[a-z][a-z0-9_]{0,31}`
    pub key: String,
    pub label: String,
    pub required: bool,
    pub max_length: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationProfile {
    pub enabled: bool,
    pub email: FieldRequirement,
    pub organization: FieldRequirement,
    pub custom_fields: Vec<CustomRegistrationField>,
    pub token_ttl_hours: u32,
    pub token_max_uses: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleMappingProfile {
    /// External role token (e.g. `DOCTOR`) → generic meeting role. Never OWNER.
    pub external_role_mapping: BTreeMap<String, Role>,
}

/// The complete effective profile policy of a space / conference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfilePolicy {
    pub recording: RecordingProfile,
    pub transcription: TranscriptionProfile,
    pub intelligence: IntelligenceProfile,
    pub data_sharing: DataSharingProfile,
    pub exports: ExportProfile,
    pub access: AccessProfile,
    pub audit: AuditProfile,
    pub stage: StageProfile,
    pub registration: RegistrationProfile,
    pub roles: RoleMappingProfile,
}

/// Effective policy plus provenance.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedProfile {
    pub profile: MeetingProfile,
    pub meeting_type: MeetingType,
    pub security_profile: SecurityProfile,
    pub policy: ProfilePolicy,
    /// A stored layer failed validation → restrictive fallback is in force.
    pub misconfigured: bool,
    /// Override fields that a ceiling clamped back (visible to administrators).
    pub clamped_fields: Vec<String>,
}

fn healthcare_role_mapping() -> BTreeMap<String, Role> {
    [
        ("DOCTOR", Role::Host),
        ("PATIENT", Role::Participant),
        ("NURSE", Role::Presenter),
        ("CAREGIVER", Role::Participant),
        ("INTERPRETER", Role::Presenter),
        ("SPECIALIST", Role::CoHost),
        ("HOSPITAL_REPRESENTATIVE", Role::Viewer),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

impl ProfilePolicy {
    /// Base defaults (STANDARD meeting).
    pub fn base() -> Self {
        Self {
            recording: RecordingProfile {
                allowed: true,
                consent_mode: ConsentMode::NoticeOnly,
                withdrawal_mode: WithdrawalMode::StopFutureProcessing,
                retention_days: 30,
                manual_delete_allowed: true,
                layout: "grid".into(),
            },
            transcription: TranscriptionProfile {
                allowed: true,
                consent_mode: ConsentMode::NoticeOnly,
                withdrawal_mode: WithdrawalMode::StopFutureProcessing,
                location: ProcessingLocation::ExternalAllowed,
                retention_days: 30,
            },
            intelligence: IntelligenceProfile {
                location: ProcessingLocation::ExternalAllowed,
                consent_required: true,
                retention_days: 30,
            },
            data_sharing: DataSharingProfile {
                allowed: false,
                consent_required: true,
            },
            exports: ExportProfile {
                chat_export_allowed: true,
                attendance_report_allowed: true,
                poll_report_allowed: true,
                retention_days: 30,
            },
            access: AccessProfile {
                public_access_allowed: true,
                waiting_room_forced: false,
                guest_session_ttl_secs: 4 * 60 * 60,
                require_external_context: false,
            },
            audit: AuditProfile { enhanced: false },
            stage: StageProfile {
                max_stage_participants: 0,
                max_presenters: 0,
                viewer_capacity: 0,
                backstage_enabled: false,
            },
            registration: RegistrationProfile {
                enabled: false,
                email: FieldRequirement::Optional,
                organization: FieldRequirement::Disabled,
                custom_fields: vec![],
                token_ttl_hours: 72,
                token_max_uses: 5,
            },
            roles: RoleMappingProfile {
                external_role_mapping: BTreeMap::new(),
            },
        }
    }

    /// Restrictive fallback used when configuration cannot be trusted.
    pub fn restrictive() -> Self {
        let mut p = Self::base();
        p.recording.allowed = false;
        p.recording.consent_mode = ConsentMode::ExplicitConsentRequired;
        p.recording.retention_days = 7;
        p.transcription.allowed = false;
        p.transcription.consent_mode = ConsentMode::ExplicitConsentRequired;
        p.transcription.location = ProcessingLocation::Disabled;
        p.intelligence.location = ProcessingLocation::Disabled;
        p.data_sharing.allowed = false;
        p.exports = ExportProfile {
            chat_export_allowed: false,
            attendance_report_allowed: false,
            poll_report_allowed: false,
            retention_days: 7,
        };
        p.access = AccessProfile {
            public_access_allowed: false,
            waiting_room_forced: true,
            guest_session_ttl_secs: 60 * 60,
            require_external_context: true,
        };
        p.audit.enhanced = true;
        p.registration.enabled = false;
        p
    }

    /// Meeting-type layer (WEBINAR / TOWN_HALL large-audience defaults).
    fn apply_meeting_type(&mut self, t: MeetingType) {
        match t {
            MeetingType::Webinar => {
                self.stage = StageProfile {
                    max_stage_participants: 25,
                    max_presenters: 10,
                    viewer_capacity: 1_000,
                    backstage_enabled: true,
                };
                self.registration.enabled = true;
                self.registration.organization = FieldRequirement::Optional;
            }
            MeetingType::TownHall => {
                self.stage = StageProfile {
                    max_stage_participants: 10,
                    max_presenters: 5,
                    viewer_capacity: 10_000,
                    backstage_enabled: true,
                };
                self.registration.enabled = true;
            }
            MeetingType::Healthcare => self.apply_healthcare_defaults(),
            MeetingType::Standard | MeetingType::Training => {}
        }
    }

    fn apply_healthcare_defaults(&mut self) {
        self.recording.allowed = false;
        self.recording.consent_mode = ConsentMode::ExplicitConsentRequired;
        self.recording.retention_days = 7;
        self.transcription.allowed = false;
        self.transcription.consent_mode = ConsentMode::ExplicitConsentRequired;
        self.transcription.location = ProcessingLocation::SelfHostedOnly;
        self.transcription.retention_days = 7;
        self.intelligence.location = ProcessingLocation::Disabled;
        self.intelligence.retention_days = 7;
        self.data_sharing.allowed = false;
        self.exports.chat_export_allowed = false;
        self.exports.retention_days = 7;
        self.access = AccessProfile {
            public_access_allowed: false,
            waiting_room_forced: true,
            guest_session_ttl_secs: 2 * 60 * 60,
            require_external_context: true,
        };
        self.audit.enhanced = true;
        self.registration.enabled = false;
        self.roles.external_role_mapping = healthcare_role_mapping();
    }

    fn apply_security_profile(&mut self, s: SecurityProfile) {
        match s {
            SecurityProfile::Healthcare => self.apply_healthcare_defaults(),
            SecurityProfile::HighSecurity => {
                self.recording.allowed = false;
                self.recording.consent_mode = ConsentMode::ExplicitConsentRequired;
                self.recording.retention_days = self.recording.retention_days.min(14);
                self.transcription.allowed = false;
                self.transcription.consent_mode = ConsentMode::ExplicitConsentRequired;
                self.transcription.location = ProcessingLocation::SelfHostedOnly;
                self.intelligence.location = ProcessingLocation::Disabled;
                self.data_sharing.allowed = false;
                self.exports.chat_export_allowed = false;
                // Public entry stays configurable (organization ceiling) so existing
                // HIGH_SECURITY spaces keep their Parts A–D access semantics.
                self.access = AccessProfile {
                    public_access_allowed: self.access.public_access_allowed,
                    waiting_room_forced: true,
                    guest_session_ttl_secs: 60 * 60,
                    require_external_context: self.access.require_external_context,
                };
                self.audit.enhanced = true;
                self.registration.enabled = false;
            }
            SecurityProfile::Standard | SecurityProfile::Business | SecurityProfile::Enterprise => {
            }
        }
    }
}

/// Hard ceilings of a regulated profile (fields not listed are free).
#[derive(Debug, Clone, Default)]
struct Ceilings {
    recording_allowed: Option<bool>,
    recording_explicit: bool,
    recording_retention: Option<u32>,
    transcription_allowed: Option<bool>,
    transcription_explicit: bool,
    transcription_location: Option<ProcessingLocation>,
    transcription_retention: Option<u32>,
    intelligence_location: Option<ProcessingLocation>,
    intelligence_consent: bool,
    intelligence_retention: Option<u32>,
    data_sharing_allowed: Option<bool>,
    data_sharing_consent: bool,
    chat_export: Option<bool>,
    attendance_report: Option<bool>,
    poll_report: Option<bool>,
    export_retention: Option<u32>,
    public_access: Option<bool>,
    waiting_room_forced: bool,
    guest_ttl: Option<u32>,
    require_external_context: bool,
    enhanced_audit: bool,
    max_stage: Option<u32>,
    max_presenters: Option<u32>,
    registration_enabled: Option<bool>,
}

fn min_opt(a: Option<u32>, b: u32) -> Option<u32> {
    Some(a.map(|x| x.min(b)).unwrap_or(b))
}

impl Ceilings {
    fn security(profile: MeetingProfile) -> Self {
        match profile {
            MeetingProfile::Healthcare => Self {
                recording_explicit: true,
                transcription_explicit: true,
                transcription_location: Some(ProcessingLocation::SelfHostedOnly),
                intelligence_location: Some(ProcessingLocation::SelfHostedOnly),
                intelligence_consent: true,
                data_sharing_consent: true,
                public_access: Some(false),
                waiting_room_forced: true,
                guest_ttl: Some(2 * 60 * 60),
                require_external_context: true,
                enhanced_audit: true,
                registration_enabled: Some(false),
                ..Self::default()
            },
            MeetingProfile::HighSecurity => Self {
                recording_explicit: true,
                transcription_explicit: true,
                transcription_location: Some(ProcessingLocation::SelfHostedOnly),
                intelligence_location: Some(ProcessingLocation::SelfHostedOnly),
                intelligence_consent: true,
                data_sharing_consent: true,
                waiting_room_forced: true,
                guest_ttl: Some(60 * 60),
                enhanced_audit: true,
                registration_enabled: Some(false),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    /// Every value the organization sets explicitly binds lower levels.
    fn add_organization(&mut self, org: &Value, effective: &ProfilePolicy) {
        let has = |ptr: &str| org.pointer(ptr).is_some();
        let p = effective;
        if has("/recording/allowed") && !p.recording.allowed {
            self.recording_allowed = Some(false);
        }
        if has("/recording/consentMode")
            && p.recording.consent_mode == ConsentMode::ExplicitConsentRequired
        {
            self.recording_explicit = true;
        }
        if has("/recording/retentionDays") {
            self.recording_retention =
                min_opt(self.recording_retention, p.recording.retention_days);
        }
        if has("/transcription/allowed") && !p.transcription.allowed {
            self.transcription_allowed = Some(false);
        }
        if has("/transcription/consentMode")
            && p.transcription.consent_mode == ConsentMode::ExplicitConsentRequired
        {
            self.transcription_explicit = true;
        }
        if has("/transcription/location") {
            self.transcription_location = Some(
                self.transcription_location
                    .map(|l| l.min(p.transcription.location))
                    .unwrap_or(p.transcription.location),
            );
        }
        if has("/transcription/retentionDays") {
            self.transcription_retention =
                min_opt(self.transcription_retention, p.transcription.retention_days);
        }
        if has("/intelligence/location") {
            self.intelligence_location = Some(
                self.intelligence_location
                    .map(|l| l.min(p.intelligence.location))
                    .unwrap_or(p.intelligence.location),
            );
        }
        if has("/intelligence/consentRequired") && p.intelligence.consent_required {
            self.intelligence_consent = true;
        }
        if has("/intelligence/retentionDays") {
            self.intelligence_retention =
                min_opt(self.intelligence_retention, p.intelligence.retention_days);
        }
        if has("/dataSharing/allowed") && !p.data_sharing.allowed {
            self.data_sharing_allowed = Some(false);
        }
        if has("/dataSharing/consentRequired") && p.data_sharing.consent_required {
            self.data_sharing_consent = true;
        }
        if has("/exports/chatExportAllowed") && !p.exports.chat_export_allowed {
            self.chat_export = Some(false);
        }
        if has("/exports/attendanceReportAllowed") && !p.exports.attendance_report_allowed {
            self.attendance_report = Some(false);
        }
        if has("/exports/pollReportAllowed") && !p.exports.poll_report_allowed {
            self.poll_report = Some(false);
        }
        if has("/exports/retentionDays") {
            self.export_retention = min_opt(self.export_retention, p.exports.retention_days);
        }
        if has("/access/publicAccessAllowed") && !p.access.public_access_allowed {
            self.public_access = Some(false);
        }
        if has("/access/waitingRoomForced") && p.access.waiting_room_forced {
            self.waiting_room_forced = true;
        }
        if has("/access/guestSessionTtlSecs") {
            self.guest_ttl = min_opt(self.guest_ttl, p.access.guest_session_ttl_secs);
        }
        if has("/access/requireExternalContext") && p.access.require_external_context {
            self.require_external_context = true;
        }
        if has("/audit/enhanced") && p.audit.enhanced {
            self.enhanced_audit = true;
        }
        if has("/stage/maxStageParticipants") && p.stage.max_stage_participants > 0 {
            self.max_stage = min_opt(self.max_stage, p.stage.max_stage_participants);
        }
        if has("/stage/maxPresenters") && p.stage.max_presenters > 0 {
            self.max_presenters = min_opt(self.max_presenters, p.stage.max_presenters);
        }
        if has("/registration/enabled") && !p.registration.enabled {
            self.registration_enabled = Some(false);
        }
    }

    /// Clamp `p` to the ceilings; returns the names of clamped fields.
    fn clamp(&self, p: &mut ProfilePolicy) -> Vec<String> {
        let mut clamped = vec![];
        let mut note = |name: &str, changed: bool| {
            if changed {
                clamped.push(name.to_string());
            }
        };
        fn cap_bool(v: &mut bool, ceiling: Option<bool>) -> bool {
            match ceiling {
                Some(false) if *v => {
                    *v = false;
                    true
                }
                _ => false,
            }
        }
        fn force_true(v: &mut bool, force: bool) -> bool {
            if force && !*v {
                *v = true;
                true
            } else {
                false
            }
        }
        fn cap_num(v: &mut u32, ceiling: Option<u32>) -> bool {
            match ceiling {
                // 0 means "not limited" for stage limits; a ceiling replaces it.
                Some(c) if *v > c || (*v == 0 && c > 0) => {
                    *v = c;
                    true
                }
                _ => false,
            }
        }
        fn cap_loc(v: &mut ProcessingLocation, ceiling: Option<ProcessingLocation>) -> bool {
            match ceiling {
                Some(c) if *v > c => {
                    *v = c;
                    true
                }
                _ => false,
            }
        }
        fn explicit(v: &mut ConsentMode, force: bool) -> bool {
            if force && *v != ConsentMode::ExplicitConsentRequired {
                *v = ConsentMode::ExplicitConsentRequired;
                true
            } else {
                false
            }
        }
        note(
            "recording.allowed",
            cap_bool(&mut p.recording.allowed, self.recording_allowed),
        );
        note(
            "recording.consentMode",
            explicit(&mut p.recording.consent_mode, self.recording_explicit),
        );
        note(
            "recording.retentionDays",
            cap_num(&mut p.recording.retention_days, self.recording_retention),
        );
        note(
            "transcription.allowed",
            cap_bool(&mut p.transcription.allowed, self.transcription_allowed),
        );
        note(
            "transcription.consentMode",
            explicit(
                &mut p.transcription.consent_mode,
                self.transcription_explicit,
            ),
        );
        note(
            "transcription.location",
            cap_loc(&mut p.transcription.location, self.transcription_location),
        );
        note(
            "transcription.retentionDays",
            cap_num(
                &mut p.transcription.retention_days,
                self.transcription_retention,
            ),
        );
        note(
            "intelligence.location",
            cap_loc(&mut p.intelligence.location, self.intelligence_location),
        );
        note(
            "intelligence.consentRequired",
            force_true(
                &mut p.intelligence.consent_required,
                self.intelligence_consent,
            ),
        );
        note(
            "intelligence.retentionDays",
            cap_num(
                &mut p.intelligence.retention_days,
                self.intelligence_retention,
            ),
        );
        note(
            "dataSharing.allowed",
            cap_bool(&mut p.data_sharing.allowed, self.data_sharing_allowed),
        );
        note(
            "dataSharing.consentRequired",
            force_true(
                &mut p.data_sharing.consent_required,
                self.data_sharing_consent,
            ),
        );
        note(
            "exports.chatExportAllowed",
            cap_bool(&mut p.exports.chat_export_allowed, self.chat_export),
        );
        note(
            "exports.attendanceReportAllowed",
            cap_bool(
                &mut p.exports.attendance_report_allowed,
                self.attendance_report,
            ),
        );
        note(
            "exports.pollReportAllowed",
            cap_bool(&mut p.exports.poll_report_allowed, self.poll_report),
        );
        note(
            "exports.retentionDays",
            cap_num(&mut p.exports.retention_days, self.export_retention),
        );
        note(
            "access.publicAccessAllowed",
            cap_bool(&mut p.access.public_access_allowed, self.public_access),
        );
        note(
            "access.waitingRoomForced",
            force_true(&mut p.access.waiting_room_forced, self.waiting_room_forced),
        );
        note(
            "access.guestSessionTtlSecs",
            cap_num(&mut p.access.guest_session_ttl_secs, self.guest_ttl),
        );
        note(
            "access.requireExternalContext",
            force_true(
                &mut p.access.require_external_context,
                self.require_external_context,
            ),
        );
        note(
            "audit.enhanced",
            force_true(&mut p.audit.enhanced, self.enhanced_audit),
        );
        note(
            "stage.maxStageParticipants",
            cap_num(&mut p.stage.max_stage_participants, self.max_stage),
        );
        note(
            "stage.maxPresenters",
            cap_num(&mut p.stage.max_presenters, self.max_presenters),
        );
        note(
            "registration.enabled",
            cap_bool(&mut p.registration.enabled, self.registration_enabled),
        );
        // External roles never map to OWNER (ownership is not delegable).
        for role in p.roles.external_role_mapping.values_mut() {
            if *role == Role::Owner {
                *role = Role::Host;
                clamped.push("roles.externalRoleMapping".into());
            }
        }
        clamped
    }
}

/// Deep-merge `patch` into `base` (objects merge, everything else replaces).
pub fn merge_json(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => {
                        merge_json(existing, v)
                    }
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

fn is_empty_layer(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

/// Apply one sparse layer strictly (unknown keys / wrong types are rejected).
fn apply_layer(policy: &ProfilePolicy, layer: &Value) -> Result<ProfilePolicy, String> {
    if is_empty_layer(layer) {
        return Ok(policy.clone());
    }
    if !layer.is_object() {
        return Err("profile policy must be an object".into());
    }
    let mut json = serde_json::to_value(policy).map_err(|e| e.to_string())?;
    merge_json(&mut json, layer);
    let merged: ProfilePolicy = serde_json::from_value(json).map_err(|e| e.to_string())?;
    validate(&merged)?;
    Ok(merged)
}

/// Value-level validation of a merged policy.
pub fn validate(p: &ProfilePolicy) -> Result<(), String> {
    let days = [
        p.recording.retention_days,
        p.transcription.retention_days,
        p.intelligence.retention_days,
        p.exports.retention_days,
    ];
    if days.iter().any(|d| *d == 0 || *d > 3650) {
        return Err("retentionDays must be between 1 and 3650".into());
    }
    if !["grid", "speaker"].contains(&p.recording.layout.as_str()) {
        return Err("recording.layout must be grid or speaker".into());
    }
    if !(300..=24 * 60 * 60).contains(&p.access.guest_session_ttl_secs) {
        return Err("access.guestSessionTtlSecs must be between 300 and 86400".into());
    }
    if p.stage.max_stage_participants > 500 || p.stage.max_presenters > 500 {
        return Err("stage limits must be at most 500".into());
    }
    if !(1..=24 * 30).contains(&p.registration.token_ttl_hours)
        || !(1..=100).contains(&p.registration.token_max_uses)
    {
        return Err("registration token limits are out of range".into());
    }
    if p.registration.custom_fields.len() > 10 {
        return Err("at most 10 custom registration fields".into());
    }
    let key_ok = |k: &str| {
        let mut chars = k.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
            && k.len() <= 32
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let mut keys = std::collections::HashSet::new();
    for f in &p.registration.custom_fields {
        if !key_ok(&f.key) || !keys.insert(f.key.clone()) {
            return Err(format!("invalid custom field key '{}'", f.key));
        }
        if f.label.trim().is_empty() || f.label.len() > 80 || !(1..=500).contains(&f.max_length) {
            return Err(format!("invalid custom field '{}'", f.key));
        }
    }
    for k in p.roles.external_role_mapping.keys() {
        if k.is_empty()
            || k.len() > 32
            || !k
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!("invalid external role '{k}'"));
        }
    }
    Ok(())
}

/// Validate a sparse override patch (used by the PATCH endpoint).
pub fn validate_patch(patch: &Value) -> Result<(), String> {
    apply_layer(&ProfilePolicy::base(), patch).map(|_| ())
}

pub fn profile_of(meeting_type: MeetingType, security: SecurityProfile) -> MeetingProfile {
    match (meeting_type, security) {
        (MeetingType::Healthcare, _) | (_, SecurityProfile::Healthcare) => {
            MeetingProfile::Healthcare
        }
        (_, SecurityProfile::HighSecurity) => MeetingProfile::HighSecurity,
        (MeetingType::Webinar, _) => MeetingProfile::Webinar,
        (MeetingType::TownHall, _) => MeetingProfile::TownHall,
        _ => MeetingProfile::Standard,
    }
}

/// The resolver: all inputs are authoritative stored state.
pub struct MeetingProfileResolver;

impl MeetingProfileResolver {
    pub fn resolve(
        meeting_type: MeetingType,
        security: SecurityProfile,
        organization: Option<&Value>,
        space: &Value,
        conference: Option<&Value>,
    ) -> ResolvedProfile {
        let profile = profile_of(meeting_type, security);
        let mut policy = ProfilePolicy::base();
        policy.apply_meeting_type(meeting_type);
        policy.apply_security_profile(security);
        let mut ceilings = Ceilings::security(profile);
        let restrictive = |clamped: Vec<String>| ResolvedProfile {
            profile,
            meeting_type,
            security_profile: security,
            policy: ProfilePolicy::restrictive(),
            misconfigured: true,
            clamped_fields: clamped,
        };
        if let Some(org) = organization {
            match apply_layer(&policy, org) {
                Ok(p) => {
                    ceilings.add_organization(org, &p);
                    policy = p;
                }
                Err(e) => {
                    tracing::error!(error = %e, "organization profile policy is misconfigured; restrictive fallback applied");
                    return restrictive(vec![]);
                }
            }
        }
        // The org / security ceilings already hold before lower overrides apply.
        ceilings.clamp(&mut policy);
        for (name, layer) in [("space", Some(space)), ("conference", conference)] {
            let Some(layer) = layer else { continue };
            match apply_layer(&policy, layer) {
                Ok(p) => policy = p,
                Err(e) => {
                    tracing::error!(layer = name, error = %e, "profile policy is misconfigured; restrictive fallback applied");
                    return restrictive(vec![]);
                }
            }
        }
        let clamped_fields = ceilings.clamp(&mut policy);
        ResolvedProfile {
            profile,
            meeting_type,
            security_profile: security,
            policy,
            misconfigured: false,
            clamped_fields,
        }
    }

    pub fn for_space(
        space: &meeting_spaces::Model,
        organization: Option<&Value>,
        conference: Option<&conference_sessions::Model>,
    ) -> ResolvedProfile {
        Self::resolve(
            space.meeting_type,
            space.security_profile,
            organization,
            &space.profile_policy,
            conference.map(|c| &c.profile_policy),
        )
    }

    /// Load the organization layer and resolve.
    pub async fn load<C: sea_orm::ConnectionTrait>(
        conn: &C,
        space: &meeting_spaces::Model,
        conference: Option<&conference_sessions::Model>,
    ) -> crate::error::ApiResult<ResolvedProfile> {
        let org = match space.organization_id {
            Some(org_id) => {
                use sea_orm::EntityTrait;
                crate::models::organizations::Entity::find_by_id(org_id)
                    .one(conn)
                    .await?
                    .map(|o| o.profile_policy)
            }
            None => None,
        };
        Ok(Self::for_space(space, org.as_ref(), conference))
    }
}

impl ResolvedProfile {
    /// Map an external (e.g. clinical) role through policy. Unknown roles map to
    /// `None` (the caller falls back to the space default); never OWNER.
    pub fn map_external_role(&self, external: &str) -> Option<Role> {
        let key = external.trim().to_ascii_uppercase();
        self.policy
            .roles
            .external_role_mapping
            .get(&key)
            .copied()
            .map(|r| if r == Role::Owner { Role::Host } else { r })
    }

    pub fn is_regulated(&self) -> bool {
        matches!(
            self.profile,
            MeetingProfile::Healthcare | MeetingProfile::HighSecurity
        )
    }
}

/// Empty sparse layer.
pub fn empty_layer() -> Value {
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resolve(
        t: MeetingType,
        s: SecurityProfile,
        org: Option<Value>,
        space: Value,
    ) -> ResolvedProfile {
        MeetingProfileResolver::resolve(t, s, org.as_ref(), &space, None)
    }

    #[test]
    fn webinar_and_town_hall_select_large_audience_defaults() {
        let w = resolve(
            MeetingType::Webinar,
            SecurityProfile::Standard,
            None,
            json!({}),
        );
        assert_eq!(w.profile, MeetingProfile::Webinar);
        assert_eq!(w.policy.stage.max_stage_participants, 25);
        assert!(w.policy.stage.backstage_enabled && w.policy.registration.enabled);
        let t = resolve(
            MeetingType::TownHall,
            SecurityProfile::Standard,
            None,
            json!({}),
        );
        assert!(t.policy.stage.max_stage_participants < w.policy.stage.max_stage_participants);
        assert!(t.policy.stage.viewer_capacity > w.policy.stage.viewer_capacity);
        let s = resolve(
            MeetingType::Standard,
            SecurityProfile::Standard,
            None,
            json!({}),
        );
        assert_eq!(s.policy.stage.max_stage_participants, 0);
        assert!(!s.policy.stage.backstage_enabled);
    }

    #[test]
    fn healthcare_defaults_are_restrictive_and_regulated_fields_cannot_be_loosened() {
        let h = resolve(
            MeetingType::Healthcare,
            SecurityProfile::Standard,
            None,
            json!({}),
        );
        assert_eq!(h.profile, MeetingProfile::Healthcare);
        assert!(!h.policy.recording.allowed);
        assert!(!h.policy.transcription.allowed);
        assert_eq!(h.policy.intelligence.location, ProcessingLocation::Disabled);
        assert!(!h.policy.access.public_access_allowed && h.policy.access.waiting_room_forced);
        assert!(h.policy.audit.enhanced);
        assert_eq!(
            h.map_external_role("doctor"),
            Some(Role::Host),
            "clinical roles map into generic roles"
        );
        // The space may opt into recording, but never into notice-only consent,
        // public access or external AI.
        let opted = resolve(
            MeetingType::Healthcare,
            SecurityProfile::Standard,
            None,
            json!({
                "recording": {"allowed": true, "consentMode": "NOTICE_ONLY"},
                "access": {"publicAccessAllowed": true, "waitingRoomForced": false, "guestSessionTtlSecs": 86400},
                "intelligence": {"location": "EXTERNAL_ALLOWED"}
            }),
        );
        assert!(opted.policy.recording.allowed);
        assert_eq!(
            opted.policy.recording.consent_mode,
            ConsentMode::ExplicitConsentRequired
        );
        assert!(!opted.policy.access.public_access_allowed);
        assert!(opted.policy.access.waiting_room_forced);
        assert_eq!(opted.policy.access.guest_session_ttl_secs, 2 * 60 * 60);
        assert_eq!(
            opted.policy.intelligence.location,
            ProcessingLocation::SelfHostedOnly
        );
        assert!(opted
            .clamped_fields
            .contains(&"recording.consentMode".to_string()));
    }

    #[test]
    fn organization_values_become_ceilings() {
        let org = json!({"recording": {"allowed": false, "retentionDays": 10}, "intelligence": {"location": "DISABLED"}});
        let r = resolve(
            MeetingType::Standard,
            SecurityProfile::Standard,
            Some(org),
            json!({"recording": {"allowed": true, "retentionDays": 365}, "intelligence": {"location": "EXTERNAL_ALLOWED"}}),
        );
        assert!(!r.policy.recording.allowed);
        assert_eq!(r.policy.recording.retention_days, 10);
        assert_eq!(r.policy.intelligence.location, ProcessingLocation::Disabled);
        // A permissive organization value does not prevent a stricter space.
        let r = resolve(
            MeetingType::Standard,
            SecurityProfile::Standard,
            Some(json!({"recording": {"retentionDays": 90}})),
            json!({"recording": {"retentionDays": 5}}),
        );
        assert_eq!(r.policy.recording.retention_days, 5);
    }

    #[test]
    fn conference_override_applies_after_space_and_is_clamped() {
        let r = MeetingProfileResolver::resolve(
            MeetingType::Standard,
            SecurityProfile::HighSecurity,
            None,
            &json!({"recording": {"allowed": true}}),
            Some(
                &json!({"recording": {"retentionDays": 3}, "transcription": {"location": "EXTERNAL_ALLOWED"}}),
            ),
        );
        assert!(r.policy.recording.allowed);
        assert_eq!(r.policy.recording.retention_days, 3);
        assert_eq!(
            r.policy.transcription.location,
            ProcessingLocation::SelfHostedOnly
        );
    }

    #[test]
    fn misconfiguration_falls_back_to_restrictive() {
        let r = resolve(
            MeetingType::Standard,
            SecurityProfile::Standard,
            None,
            json!({"recording": {"allowed": "yes"}}),
        );
        assert!(r.misconfigured);
        assert!(!r.policy.recording.allowed && !r.policy.access.public_access_allowed);
        let r = resolve(
            MeetingType::Standard,
            SecurityProfile::Standard,
            Some(json!({"unknownKey": 1})),
            json!({}),
        );
        assert!(r.misconfigured);
        assert!(validate_patch(&json!({"recording": {"retentionDays": 0}})).is_err());
        assert!(validate_patch(&json!({"recording": {"retentionDays": 30}})).is_ok());
        assert!(
            validate_patch(&json!({"roles": {"externalRoleMapping": {"doctor": "HOST"}}})).is_err()
        );
    }

    #[test]
    fn external_roles_never_map_to_owner() {
        let r = resolve(
            MeetingType::Healthcare,
            SecurityProfile::Standard,
            None,
            json!({"roles": {"externalRoleMapping": {"DOCTOR": "OWNER"}}}),
        );
        assert_eq!(r.map_external_role("DOCTOR"), Some(Role::Host));
        assert_eq!(r.map_external_role("SURGEON"), None);
    }
}
