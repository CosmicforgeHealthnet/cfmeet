//! Pre-join contract (Part F §16–§19).
//!
//! `GET /api/v2/conferences/{id}/pre-join` resolves everything a pre-join screen needs
//! *before* the media join, for any caller (anonymous included), without leaking
//! private organizer or sensitive meeting information:
//!
//! - the MeetingSpace / occurrence, state, joinability;
//! - access requirements (authentication, invitation, passcode, external authority) and
//!   the waiting-room expectation for this caller;
//! - recording / transcription / AI notices and the explicit consents required before
//!   admission (`meeting_consents` authority);
//! - the media defaults the caller would get (role → effective media);
//! - display-name editability (a display name is presentation, never identity);
//! - device capability hooks — device preview, selection and testing are client-side;
//!   no device id, label or fingerprint is ever collected.

use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, Statement};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{
        AccessPolicy, ConferenceStatus, MediaCapturePolicy, MeetingType, RecordOrigin, Role,
        SecurityProfile, WaitingRoomPolicy,
    },
    dto::v2::JoinPermissions,
    error::{ApiError, ApiResult, ErrorCode},
    models::{organizations, users},
    services::{
        actor::ActorContext,
        meeting_core::{
            conference_authority, find_conference, find_space, not_found_conference,
            own_participant, space_role, Core,
        },
        meeting_events::{action, audit, AuditActor, AuditRecord},
        meeting_permissions::{Permission, PermissionContext},
    },
};

use super::{
    collaboration_disabled, effective_policy,
    media::{effective_media, MediaRestrictions},
    policy::{ConsentKind, ScreenSharePolicy},
};

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinResponse {
    pub meeting_space_id: Uuid,
    pub conference_id: Uuid,
    /// Hidden for sensitive (healthcare / high-security) meetings until authorized.
    pub title: Option<String>,
    pub meeting_type: MeetingType,
    pub conference_status: ConferenceStatus,
    pub scheduled_start_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Media join is possible right now (LIVE).
    pub joinable: bool,
    pub organizer: Option<PreJoinOrganizer>,
    pub organization: Option<PreJoinOrganization>,
    pub access: PreJoinAccess,
    pub notices: PreJoinNotices,
    pub consents: PreJoinConsents,
    pub media: PreJoinMedia,
    pub display_name: PreJoinDisplayName,
    pub devices: PreJoinDevices,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinOrganizer {
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinOrganization {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinAccess {
    pub access_policy: AccessPolicy,
    pub authentication_required: bool,
    pub invitation_required: bool,
    pub passcode_required: bool,
    /// Decided by an external authority (e.g. a healthcare appointment).
    pub external_authorization: bool,
    pub waiting_room_expected: bool,
    pub already_authorized: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinNotices {
    pub recording_policy: MediaCapturePolicy,
    pub recording_active: bool,
    pub transcription_policy: MediaCapturePolicy,
    pub transcription_active: bool,
    pub ai_notice: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinConsents {
    /// Must be granted (join request `consents`) before admission.
    pub required: Vec<ConsentKind>,
    pub version: String,
    /// Already granted by the caller's participant (rejoin).
    pub granted: Vec<ConsentKind>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinMedia {
    /// The role the caller is expected to join with.
    pub role: Role,
    pub permissions: JoinPermissions,
    pub audio_locked: bool,
    pub video_locked: bool,
    pub screen_share_policy: ScreenSharePolicy,
    pub screen_share_eligible: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinDisplayName {
    pub editable: bool,
    /// Suggested name (profile name for registered users).
    pub suggested: Option<String>,
    /// Always false: a display name never selects identity, role or participant.
    pub identity_bearing: bool,
}

/// Client-side device contract. The backend never needs device ids for authorization.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreJoinDevices {
    pub camera_preview: bool,
    pub camera_selection: bool,
    pub microphone_selection: bool,
    pub microphone_test: bool,
    /// Speaker selection depends on the platform (setSinkId support).
    pub speaker_selection: String,
    /// Background blur / replacement run client-side only.
    pub background_effects: String,
    /// Device ids, labels and fingerprints are never sent or stored.
    pub device_identifiers_collected: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsentGrant {
    #[serde(rename = "type")]
    pub consent_type: ConsentKind,
    pub version: String,
}

fn sensitive(profile: SecurityProfile) -> bool {
    matches!(
        profile,
        SecurityProfile::Healthcare | SecurityProfile::HighSecurity
    )
}

pub struct PreJoinService {
    core: Core,
}

impl PreJoinService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    pub async fn resolve(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<PreJoinResponse> {
        if !self.core.flags.collaboration_v2_enabled {
            return Err(collaboration_disabled());
        }
        let conference = find_conference(&self.core.db, conference_id).await?;
        if conference.origin == RecordOrigin::Legacy {
            return Err(not_found_conference());
        }
        let space = find_space(&self.core.db, conference.meeting_space_id).await?;
        let authority = conference_authority(&self.core.db, &space, conference_id, ctx).await?;
        let authorized = authority.is_some();
        let own = own_participant(&self.core.db, conference_id, ctx, false).await?;
        let hide = sensitive(space.security_profile) && !authorized;
        let policy = effective_policy(&space, &conference);
        let pctx = PermissionContext::for_space(&space);

        let persistent = match ctx.registered() {
            Some(r) => space_role(&self.core.db, &space, r)
                .await
                .ok()
                .flatten()
                .map(|(role, _)| role),
            None => None,
        };
        let webinar = matches!(
            space.meeting_type,
            MeetingType::Webinar | MeetingType::TownHall
        );
        let default_role = if webinar {
            Role::Viewer
        } else {
            space.default_role
        };
        let role = authority
            .as_ref()
            .map(|a| a.role)
            .or_else(|| own.as_ref().map(|p| p.role))
            .or(persistent)
            .unwrap_or(default_role);
        let restrictions = own.as_ref().map(MediaRestrictions::of).unwrap_or_default();
        let media = effective_media(&pctx, &policy, role, restrictions);

        let organizer_visible = authorized
            || (!sensitive(space.security_profile)
                && matches!(
                    space.access_policy,
                    AccessPolicy::Public
                        | AccessPolicy::PublicPasscode
                        | AccessPolicy::Authenticated
                ));
        // An environment-owned meeting has no CF Meet organizer account to show.
        let organizer = match space.owner_user_id.filter(|_| organizer_visible) {
            Some(owner) => users::Entity::find_by_id(owner)
                .one(&self.core.db)
                .await?
                .map(|u| PreJoinOrganizer {
                    display_name: u.username,
                }),
            None => None,
        };
        let organization = match (space.organization_id, hide) {
            (Some(id), false) => organizations::Entity::find_by_id(id)
                .one(&self.core.db)
                .await?
                .map(|o| PreJoinOrganization {
                    id: o.id,
                    name: o.name,
                }),
            _ => None,
        };
        let exempt = pctx.allows(role, Permission::ParticipantAdmit);
        let waiting_room_expected = !authorized
            && !exempt
            && match space.waiting_room_policy {
                WaitingRoomPolicy::Off => false,
                WaitingRoomPolicy::NonMembersOnly => persistent.is_none(),
                WaitingRoomPolicy::GuestsOnly | WaitingRoomPolicy::UnauthenticatedOnly => {
                    ctx.registered().is_none()
                }
                _ => true,
            };
        let granted = match &own {
            Some(p) => granted_consents(&self.core.db, p.id, &policy.consent_version).await?,
            None => vec![],
        };
        Ok(PreJoinResponse {
            meeting_space_id: space.id,
            conference_id,
            title: (!hide).then(|| space.title.clone()),
            meeting_type: space.meeting_type,
            conference_status: conference.status,
            scheduled_start_at: conference.scheduled_start_at,
            joinable: conference.status == ConferenceStatus::Live,
            organizer,
            organization,
            access: PreJoinAccess {
                access_policy: space.access_policy,
                authentication_required: matches!(
                    space.access_policy,
                    AccessPolicy::Authenticated
                        | AccessPolicy::OrganizationOnly
                        | AccessPolicy::DomainRestricted
                ) && ctx.registered().is_none(),
                invitation_required: space.access_policy == AccessPolicy::InviteOnly && !authorized,
                passcode_required: space.access_policy == AccessPolicy::PublicPasscode
                    && !authorized,
                external_authorization: matches!(
                    space.access_policy,
                    AccessPolicy::HealthcareAppointment | AccessPolicy::Custom
                ),
                waiting_room_expected,
                already_authorized: authorized,
            },
            notices: PreJoinNotices {
                recording_policy: space.recording_policy,
                recording_active: conference.recording_state != "OFF",
                transcription_policy: space.transcription_policy,
                transcription_active: conference.transcription_state != "OFF",
                ai_notice: policy.ai_notice,
            },
            consents: PreJoinConsents {
                required: policy.required_consents.clone(),
                version: policy.consent_version.clone(),
                granted,
            },
            media: PreJoinMedia {
                role,
                permissions: media.into(),
                audio_locked: restrictions.audio_locked
                    || (policy.audio_locked && !pctx.allows(role, Permission::ParticipantMute)),
                video_locked: restrictions.video_locked
                    || (policy.video_locked && !pctx.allows(role, Permission::ParticipantMute)),
                screen_share_policy: policy.screen_share_policy,
                screen_share_eligible: media.screen_share_publish,
            },
            display_name: PreJoinDisplayName {
                editable: display_name_editable(ctx, space.security_profile),
                suggested: ctx
                    .registered()
                    .map(|r| r.display_name.clone())
                    .or_else(|| own.as_ref().map(|p| p.display_name.clone())),
                identity_bearing: false,
            },
            devices: PreJoinDevices {
                camera_preview: true,
                camera_selection: true,
                microphone_selection: true,
                microphone_test: true,
                speaker_selection: "PLATFORM_DEPENDENT".into(),
                background_effects: "CLIENT_SIDE".into(),
                device_identifiers_collected: false,
            },
        })
    }
}

/// Registered users in sensitive meetings present under their profile name only.
pub fn display_name_editable(ctx: &ActorContext, profile: SecurityProfile) -> bool {
    !(ctx.registered().is_some() && sensitive(profile))
}

pub async fn granted_consents<C: ConnectionTrait>(
    conn: &C,
    participant_id: Uuid,
    version: &str,
) -> ApiResult<Vec<ConsentKind>> {
    let rows = conn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT consent_type FROM meeting_consents WHERE participant_id = $1 AND consent_version = $2 AND decision = 'GRANTED'",
            [participant_id.into(), version.into()],
        ))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get_by_index::<String>(0).ok())
        .filter_map(|s| serde_json::from_value(serde_json::Value::String(s)).ok())
        .collect())
}

/// Consents still missing for admission (`required` minus already granted minus
/// granted in this request with the current version).
pub fn missing_consents(
    required: &[ConsentKind],
    version: &str,
    already: &[ConsentKind],
    request: &[ConsentGrant],
) -> Vec<ConsentKind> {
    required
        .iter()
        .copied()
        .filter(|k| !already.contains(k))
        .filter(|k| {
            !request
                .iter()
                .any(|g| g.consent_type == *k && g.version == version)
        })
        .collect()
}

pub fn consent_required_error(missing: &[ConsentKind]) -> ApiError {
    ApiError::domain(
        ErrorCode::ConsentRequired,
        format!(
            "Explicit consent is required before joining: {}",
            missing
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

/// Record granted consents on the join transaction (idempotent per version).
pub async fn record_consents(
    txn: &DatabaseTransaction,
    meeting_space_id: Uuid,
    conference_id: Uuid,
    participant_id: Uuid,
    grants: &[ConsentKind],
    version: &str,
) -> ApiResult<()> {
    for k in grants {
        let inserted = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO meeting_consents (id, conference_id, participant_id, consent_type, consent_version, decision) \
                 VALUES ($1,$2,$3,$4,$5,'GRANTED') ON CONFLICT (participant_id, consent_type, consent_version) \
                 DO UPDATE SET decision = 'GRANTED', decided_at = now(), withdrawn_at = NULL, updated_at = now() \
                 WHERE meeting_consents.decision <> 'GRANTED'",
                [Uuid::new_v4().into(), conference_id.into(), participant_id.into(), k.as_str().into(), version.into()],
            ))
            .await?
            .rows_affected();
        if inserted > 0 {
            audit(
                txn,
                &AuditActor::participant(participant_id),
                AuditRecord {
                    meeting_space_id,
                    conference_id: Some(conference_id),
                    action: action::CONSENT_RECORDED,
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(participant_id.to_string()),
                    metadata: serde_json::json!({ "consentType": k.as_str(), "version": version }),
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_consents_respects_version_and_history() {
        let req = vec![ConsentKind::Recording, ConsentKind::AiProcessing];
        let grants = vec![ConsentGrant {
            consent_type: ConsentKind::Recording,
            version: "v2".into(),
        }];
        assert_eq!(
            missing_consents(&req, "v2", &[], &grants),
            vec![ConsentKind::AiProcessing]
        );
        assert_eq!(
            missing_consents(&req, "v3", &[], &grants),
            req,
            "old version does not count"
        );
        assert!(missing_consents(&req, "v2", &[ConsentKind::AiProcessing], &grants).is_empty());
    }
}
