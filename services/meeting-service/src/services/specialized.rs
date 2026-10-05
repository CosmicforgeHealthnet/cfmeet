//! Phase 2 Part H — specialized conference modes as policy profiles over the same
//! platform (internal services of the meeting-service).
//!
//! - [`ProfilePolicyService`]: read / patch the space-level profile override; the
//!   effective policy always comes from `MeetingProfileResolver`.
//! - [`RegistrationService`]: webinar / town-hall registration collecting only
//!   policy-approved fields. Registration issues a normal hashed, scoped, expiring,
//!   revocable `meeting_invitations` token for VIEWER — it never mints RTC grants; the
//!   token resolves through normal admission (ban / access / waiting room / lock).
//! - [`BackstageService`]: a controlled preparation context of the SAME conference
//!   (`backstage_at` on the participant + its own provider room `cfbs-<uuid>`). The
//!   single-media-context rule (Part F) routes backstage participants out of the main
//!   room; stale backstage credentials are evicted by the verified webhook and the
//!   reconciler.
//! - Stage limits ([`check_stage_capacity`]): the interactive stage stays small while
//!   the audience is subscribe-only VIEWERs.

use chrono::{Duration, Utc};
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    domain::{AccessPolicy, AdmissionState, ConferenceStatus, InvitationStatus, Role, SpaceStatus},
    error::{ApiError, ApiResult, ErrorCode},
    models::{
        conference_participants, conference_sessions, meeting_invitations, meeting_spaces,
        webinar_registrations,
    },
    services::{
        actor::ActorContext,
        artifacts::ArtifactCtx,
        collaboration::breakouts::{MediaContext, MediaContextView},
        meeting_core::{
            find_conference, find_space, is_unique_violation, lock_conference, lock_space,
            permission_denied, space_role, validation, Core,
        },
        meeting_events::{
            action, audit, emit, emit_to, event, Audience, AuditRecord, OutboxRecord,
        },
        meeting_permissions::{space_allows, Permission, PermissionContext, SpacePermission},
        ops_metrics::{inc, OPS_METRICS as OM},
        profiles::{
            merge_json, validate_patch, FieldRequirement, MeetingProfileResolver, ResolvedProfile,
        },
        rtc_control::RtcControl,
        rtc_provider::{RoomSpec, RtcRoomRef},
        secrets,
    },
};

pub const BACKSTAGE_ROOM_PREFIX: &str = "cfbs-";

pub fn backstage_room_name(conference_id: Uuid) -> String {
    format!("{BACKSTAGE_ROOM_PREFIX}{}", conference_id.simple())
}

fn profile_denied(message: &str) -> ApiError {
    inc(&OM.profile_policy_denials);
    ApiError::domain(ErrorCode::ProfilePolicyDenied, message)
}

/// Reject access policies a profile forbids (e.g. PUBLIC for HEALTHCARE).
pub fn check_access_policy(profile: &ResolvedProfile, access: AccessPolicy) -> ApiResult<()> {
    if !profile.policy.access.public_access_allowed
        && matches!(access, AccessPolicy::Public | AccessPolicy::PublicPasscode)
    {
        return Err(profile_denied(
            "This meeting profile does not allow uncontrolled public access",
        ));
    }
    Ok(())
}

/// Ensure the backstage provider room exists (idempotent; records readiness).
pub async fn ensure_backstage_room(
    core: &Core,
    conference: &conference_sessions::Model,
) -> ApiResult<()> {
    let Some(name) = conference.backstage_room_name.clone() else {
        return Err(ApiError::domain(
            ErrorCode::BackstageAccessDenied,
            "Backstage is not open for this conference",
        ));
    };
    let room = RtcRoomRef {
        conference_id: conference.id,
        room_name: name,
    };
    match core.rtc.get_room(&room).await {
        Ok(Some(_)) if conference.backstage_ready_at.is_some() => return Ok(()),
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    core.rtc.create_room(&room, &RoomSpec::default()).await?;
    core.db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE conference_sessions SET backstage_ready_at = COALESCE(backstage_ready_at, now()) WHERE id = $1",
            [conference.id.into()],
        ))
        .await?;
    Ok(())
}

/// Stage capacity when `target` would take `to` (VIEWERs are the audience; the host
/// hierarchy is always allowed on stage).
pub async fn check_stage_capacity<C: ConnectionTrait>(
    conn: &C,
    profile: &ResolvedProfile,
    conference_id: Uuid,
    target: &conference_participants::Model,
    to: Role,
) -> ApiResult<()> {
    let stage = &profile.policy.stage;
    if to == Role::Viewer || matches!(to, Role::Owner | Role::Host | Role::CoHost) {
        return Ok(());
    }
    let count = |roles: Vec<Role>| {
        conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::AdmissionState.eq(AdmissionState::Admitted))
            .filter(conference_participants::Column::Role.is_in(roles))
            .filter(conference_participants::Column::Id.ne(target.id))
            .count(conn)
    };
    if stage.max_stage_participants > 0 && target.role == Role::Viewer {
        let on_stage = count(vec![
            Role::Owner,
            Role::Host,
            Role::CoHost,
            Role::Moderator,
            Role::Presenter,
            Role::Participant,
        ])
        .await?;
        if on_stage >= stage.max_stage_participants as u64 {
            inc(&OM.stage_capacity_rejections);
            return Err(ApiError::domain(
                ErrorCode::StageCapacityReached,
                "The interactive stage is full",
            ));
        }
    }
    if stage.max_presenters > 0 && to == Role::Presenter && target.role != Role::Presenter {
        let presenters = count(vec![Role::Presenter]).await?;
        if presenters >= stage.max_presenters as u64 {
            inc(&OM.stage_capacity_rejections);
            return Err(ApiError::domain(
                ErrorCode::StageCapacityReached,
                "The presenter limit is reached",
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Profile policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfilePolicyUpdateRequest {
    /// Optimistic concurrency (`profilePolicyVersion` from the last read).
    pub profile_policy_version: i32,
    /// Sparse override (validated against the profile schema; unknown keys rejected).
    #[schema(value_type = Object)]
    pub changes: Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePolicyResponse {
    pub meeting_space_id: Uuid,
    pub profile_policy_version: i32,
    /// The stored sparse space override.
    #[schema(value_type = Object)]
    pub overrides: Value,
    pub effective: ResolvedProfile,
}

pub struct ProfilePolicyService {
    core: Core,
}

impl ProfilePolicyService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    async fn authorize(
        &self,
        ctx: &ActorContext,
        space: &meeting_spaces::Model,
        permission: SpacePermission,
    ) -> ApiResult<()> {
        let actor = ctx.require_registered()?;
        let role = space_role(&self.core.db, space, actor)
            .await?
            .map(|(r, _)| r)
            .ok_or_else(|| permission_denied("You are not a member of this meeting"))?;
        if !space_allows(role, permission, &PermissionContext::for_space(space)) {
            return Err(permission_denied(
                "You cannot manage this meeting's profile policy",
            ));
        }
        Ok(())
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<ProfilePolicyResponse> {
        let space = find_space(&self.core.db, space_id).await?;
        self.authorize(ctx, &space, SpacePermission::ViewMembers)
            .await?;
        let effective = MeetingProfileResolver::load(&self.core.db, &space, None).await?;
        Ok(ProfilePolicyResponse {
            meeting_space_id: space.id,
            profile_policy_version: space.profile_policy_version,
            overrides: space.profile_policy.clone(),
            effective,
        })
    }

    pub async fn update(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: &ProfilePolicyUpdateRequest,
    ) -> ApiResult<ProfilePolicyResponse> {
        if !req.changes.is_object() {
            return Err(validation("changes must be an object"));
        }
        let current = find_space(&self.core.db, space_id).await?;
        self.authorize(ctx, &current, SpacePermission::Configure)
            .await?;
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, space_id).await?;
        if space.profile_policy_version != req.profile_policy_version {
            return Err(ApiError::domain(
                ErrorCode::MeetingVersionConflict,
                "The profile policy was modified; reload and retry",
            ));
        }
        let mut merged = space.profile_policy.clone();
        if !merged.is_object() {
            merged = json!({});
        }
        merge_json(&mut merged, &req.changes);
        validate_patch(&merged).map_err(validation)?;
        let mut candidate = space.clone();
        candidate.profile_policy = merged.clone();
        let org = match space.organization_id {
            Some(o) => crate::models::organizations::Entity::find_by_id(o)
                .one(&txn)
                .await?
                .map(|o| o.profile_policy),
            None => None,
        };
        let effective = MeetingProfileResolver::for_space(&candidate, org.as_ref(), None);
        check_access_policy(&effective, space.access_policy)?;
        let mut am: meeting_spaces::ActiveModel = space.clone().into();
        am.profile_policy = Set(merged.clone());
        am.profile_policy_version = Set(space.profile_policy_version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &crate::services::meeting_events::AuditActor::from_ctx(ctx),
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: None,
                action: action::PROFILE_POLICY_UPDATE,
                target_type: Some("MEETING_SPACE"),
                target_ref: Some(space.id.to_string()),
                metadata: json!({
                    "changedKeys": req.changes.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
                    "clampedFields": effective.clamped_fields,
                }),
            },
        )
        .await?;
        let live = conference_sessions::Entity::find()
            .filter(conference_sessions::Column::MeetingSpaceId.eq(space.id))
            .filter(conference_sessions::Column::Status.eq(ConferenceStatus::Live))
            .all(&txn)
            .await?;
        for c in &live {
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "meeting_space",
                    aggregate_id: space.id,
                    event_type: event::PROFILE_POLICY_CHANGED,
                    meeting_space_id: Some(space.id),
                    conference_id: Some(c.id),
                    payload: json!({
                        "meetingSpaceId": space.id,
                        "profilePolicyVersion": updated.profile_policy_version,
                        "profile": effective.profile,
                    }),
                },
            )
            .await?;
        }
        txn.commit().await?;
        // A tightened policy stops processing it no longer permits (no data deleted).
        for c in &live {
            let now_effective =
                MeetingProfileResolver::load(&self.core.db, &updated, Some(c)).await?;
            if !now_effective.policy.recording.allowed {
                crate::services::artifacts::recording::RecordingService::new(self.core.clone())
                    .system_stop(c.id, "PROFILE_POLICY_RESTRICTED")
                    .await?;
            }
            if !now_effective.policy.transcription.allowed {
                crate::services::artifacts::transcription::TranscriptionService::new(
                    self.core.clone(),
                )
                .system_stop(c.id, "PROFILE_POLICY_RESTRICTED")
                .await?;
            }
        }
        Ok(ProfilePolicyResponse {
            meeting_space_id: updated.id,
            profile_policy_version: updated.profile_policy_version,
            overrides: updated.profile_policy.clone(),
            effective: MeetingProfileResolver::load(&self.core.db, &updated, None).await?,
        })
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationRequest {
    /// Register for one occurrence (optional; otherwise the whole meeting series).
    pub conference_id: Option<Uuid>,
    pub display_name: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    /// Only keys approved by the profile policy are accepted (string values).
    #[serde(default)]
    pub custom_fields: Option<serde_json::Map<String, Value>>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationCreated {
    pub registration_id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    /// Opaque admission token (shown once; only its hash is stored). Absent for an
    /// idempotent repeat.
    pub admission_token: Option<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationView {
    pub id: Uuid,
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub status: String,
    pub display_name: String,
    pub email: Option<String>,
    pub organization: Option<String>,
    #[schema(value_type = Object)]
    pub custom_fields: Value,
    pub created_at: chrono::DateTime<Utc>,
    pub cancelled_at: Option<chrono::DateTime<Utc>>,
}

impl RegistrationView {
    fn of(r: &webinar_registrations::Model) -> Self {
        Self {
            id: r.id,
            meeting_space_id: r.meeting_space_id,
            conference_id: r.conference_id,
            status: r.status.clone(),
            display_name: r.display_name.clone(),
            email: r.email.clone(),
            organization: r.organization_name.clone(),
            custom_fields: r.custom_fields.clone(),
            created_at: r.created_at,
            cancelled_at: r.cancelled_at,
        }
    }
}

fn valid_email(e: &str) -> bool {
    let parts: Vec<&str> = e.split('@').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && e.len() <= 254
        && !e.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub struct RegistrationService {
    core: Core,
}

impl RegistrationService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    fn disabled() -> ApiError {
        ApiError::domain(
            ErrorCode::RegistrationDisabled,
            "Registration is not available",
        )
    }

    pub async fn register(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        req: &RegistrationRequest,
    ) -> ApiResult<RegistrationCreated> {
        if !self.core.flags.webinar_mode_enabled {
            return Err(Self::disabled());
        }
        let space = find_space(&self.core.db, space_id)
            .await
            .map_err(|_| Self::disabled())?;
        if space.status != SpaceStatus::Active {
            return Err(Self::disabled());
        }
        let conference = match req.conference_id {
            Some(cid) => {
                let c = find_conference(&self.core.db, cid)
                    .await
                    .map_err(|_| Self::disabled())?;
                if c.meeting_space_id != space.id {
                    return Err(Self::disabled());
                }
                if matches!(
                    c.status,
                    ConferenceStatus::Ended
                        | ConferenceStatus::Cancelled
                        | ConferenceStatus::Failed
                ) {
                    return Err(ApiError::domain(
                        ErrorCode::RegistrationClosed,
                        "Registration for this session is closed",
                    ));
                }
                Some(c)
            }
            None => None,
        };
        let profile =
            MeetingProfileResolver::load(&self.core.db, &space, conference.as_ref()).await?;
        let rp = &profile.policy.registration;
        if !rp.enabled {
            inc(&OM.registrations_rejected);
            return Err(Self::disabled());
        }
        // Flood protection: per meeting space (registration is pre-identity).
        self.core
            .collab_limiter
            .check(
                crate::services::collaboration::RateAction::Registration,
                &space.id.to_string(),
            )
            .await?;
        let display_name = crate::services::admission::sanitize_display_name(&req.display_name)?;
        let field = |name: &str,
                     req_kind: FieldRequirement,
                     v: &Option<String>,
                     max: usize|
         -> ApiResult<Option<String>> {
            let v = v
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            match (req_kind, &v) {
                (FieldRequirement::Disabled, Some(_)) => Err(validation(format!(
                    "{name} is not collected for this meeting"
                ))),
                (FieldRequirement::Required, None) => {
                    Err(validation(format!("{name} is required")))
                }
                (_, Some(s)) if s.chars().count() > max => {
                    Err(validation(format!("{name} is too long")))
                }
                _ => Ok(v),
            }
        };
        let email = field("email", rp.email, &req.email, 254)?.map(|e| e.to_ascii_lowercase());
        if let Some(e) = &email {
            if !valid_email(e) {
                return Err(validation("email is not valid"));
            }
        }
        let organization = field("organization", rp.organization, &req.organization, 120)?;
        let mut custom = serde_json::Map::new();
        let provided = req.custom_fields.clone().unwrap_or_default();
        for key in provided.keys() {
            if !rp.custom_fields.iter().any(|f| &f.key == key) {
                return Err(validation(format!(
                    "customFields.{key} is not an approved field"
                )));
            }
        }
        for f in &rp.custom_fields {
            match provided.get(&f.key) {
                Some(Value::String(s)) => {
                    let s = s.trim();
                    if s.chars().count() > f.max_length as usize
                        || s.chars().any(|c| c.is_control())
                    {
                        return Err(validation(format!("customFields.{} is too long", f.key)));
                    }
                    if !s.is_empty() {
                        custom.insert(f.key.clone(), Value::String(s.to_string()));
                    } else if f.required {
                        return Err(validation(format!("customFields.{} is required", f.key)));
                    }
                }
                Some(_) => {
                    return Err(validation(format!(
                        "customFields.{} must be a string",
                        f.key
                    )))
                }
                None if f.required => {
                    return Err(validation(format!("customFields.{} is required", f.key)))
                }
                None => {}
            }
        }
        let idem = match &req.idempotency_key {
            Some(k) if k.trim().is_empty() || k.len() > 128 => {
                return Err(validation("idempotencyKey must be 1-128 characters"))
            }
            Some(k) => Some(secrets::hash_secret(&format!("{}:{k}", space.id))),
            None => None,
        };
        if let Some(d) = &idem {
            if let Some(existing) = webinar_registrations::Entity::find()
                .filter(webinar_registrations::Column::MeetingSpaceId.eq(space.id))
                .filter(webinar_registrations::Column::IdempotencyKeyDigest.eq(d.clone()))
                .one(&self.core.db)
                .await?
            {
                let inv = meeting_invitations::Entity::find_by_id(existing.invitation_id)
                    .one(&self.core.db)
                    .await?;
                return Ok(RegistrationCreated {
                    registration_id: existing.id,
                    meeting_space_id: existing.meeting_space_id,
                    conference_id: existing.conference_id,
                    admission_token: None,
                    expires_at: inv.map(|i| i.expires_at).unwrap_or_else(Utc::now),
                    created: false,
                });
            }
        }
        let now = Utc::now();
        let mut expires = now + Duration::hours(rp.token_ttl_hours as i64);
        if let Some(end) = conference.as_ref().and_then(|c| c.scheduled_end_at) {
            expires = expires
                .min(end + Duration::hours(2))
                .max(now + Duration::minutes(5));
        }
        let token = secrets::generate_bearer_secret("reg_");
        let invitation_id = Uuid::new_v4();
        let registration_id = Uuid::new_v4();
        let txn = self.core.db.begin().await?;
        meeting_invitations::ActiveModel {
            id: Set(invitation_id),
            meeting_space_id: Set(space.id),
            conference_id: Set(conference.as_ref().map(|c| c.id)),
            inviter_user_id: Set(space.owner_user_id),
            // Never bound to an account: registrants may join as guests.
            invitee_email: Set(None),
            // Registration can never select a privileged role.
            requested_role: Set(Role::Viewer),
            token_hash: Set(secrets::hash_secret(&token)),
            expires_at: Set(expires),
            use_limit: Set(Some(rp.token_max_uses as i32)),
            usage_count: Set(0),
            status: Set(InvitationStatus::Active),
            revoked_at: Set(None),
            revoked_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await?;
        let inserted = webinar_registrations::ActiveModel {
            id: Set(registration_id),
            meeting_space_id: Set(space.id),
            conference_id: Set(conference.as_ref().map(|c| c.id)),
            status: Set("REGISTERED".into()),
            display_name: Set(display_name),
            email: Set(email),
            organization_name: Set(organization),
            custom_fields: Set(Value::Object(custom)),
            user_id: Set(ctx.user_id()),
            invitation_id: Set(invitation_id),
            idempotency_key_digest: Set(idem),
            request_digest: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            cancelled_at: Set(None),
            cancelled_by: Set(None),
        }
        .insert(&txn)
        .await;
        match inserted {
            Ok(_) => {}
            Err(e) if is_unique_violation(&e) => {
                inc(&OM.registrations_rejected);
                return Err(ApiError::domain(
                    ErrorCode::ParticipantStateConflict,
                    "This registration already exists",
                ));
            }
            Err(e) => return Err(e.into()),
        }
        audit(
            &txn,
            &crate::services::meeting_events::AuditActor::from_ctx(ctx),
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: conference.as_ref().map(|c| c.id),
                action: action::REGISTRATION_CREATE,
                target_type: Some("REGISTRATION"),
                target_ref: Some(registration_id.to_string()),
                metadata: json!({ "invitationId": invitation_id }),
            },
        )
        .await?;
        if let Some(c) = &conference {
            lock_conference(&txn, c.id).await?;
            emit_to(
                &txn,
                OutboxRecord {
                    aggregate_type: "registration",
                    aggregate_id: registration_id,
                    event_type: event::REGISTRATION_CREATED,
                    meeting_space_id: Some(space.id),
                    conference_id: Some(c.id),
                    payload: json!({ "registrationId": registration_id, "conferenceId": c.id }),
                },
                Some(Audience::Permission {
                    permission: Permission::ParticipantAdmit.key().into(),
                }),
            )
            .await?;
        }
        txn.commit().await?;
        inc(&OM.registrations_created);
        Ok(RegistrationCreated {
            registration_id,
            meeting_space_id: space.id,
            conference_id: conference.as_ref().map(|c| c.id),
            admission_token: Some(token),
            expires_at: expires,
            created: true,
        })
    }

    async fn host_space(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<meeting_spaces::Model> {
        if !self.core.flags.webinar_mode_enabled {
            return Err(Self::disabled());
        }
        let space = find_space(&self.core.db, space_id).await?;
        let actor = ctx.require_registered()?;
        let role = space_role(&self.core.db, &space, actor)
            .await?
            .map(|(r, _)| r)
            .ok_or_else(|| permission_denied("You are not a member of this meeting"))?;
        if !space_allows(
            role,
            SpacePermission::ViewMembers,
            &PermissionContext::for_space(&space),
        ) {
            return Err(permission_denied("You cannot view registrations"));
        }
        Ok(space)
    }

    pub async fn list(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
    ) -> ApiResult<Vec<RegistrationView>> {
        let space = self.host_space(ctx, space_id).await?;
        Ok(webinar_registrations::Entity::find()
            .filter(webinar_registrations::Column::MeetingSpaceId.eq(space.id))
            .order_by_desc(webinar_registrations::Column::CreatedAt)
            .limit(500)
            .all(&self.core.db)
            .await?
            .iter()
            .map(RegistrationView::of)
            .collect())
    }

    async fn scoped(&self, space_id: Uuid, id: Uuid) -> ApiResult<webinar_registrations::Model> {
        webinar_registrations::Entity::find_by_id(id)
            .filter(webinar_registrations::Column::MeetingSpaceId.eq(space_id))
            .one(&self.core.db)
            .await?
            .ok_or_else(|| {
                ApiError::domain(ErrorCode::RegistrationNotFound, "Registration not found")
            })
    }

    pub async fn get(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        id: Uuid,
    ) -> ApiResult<RegistrationView> {
        let space = self.host_space(ctx, space_id).await?;
        Ok(RegistrationView::of(&self.scoped(space.id, id).await?))
    }

    pub async fn cancel(
        &self,
        ctx: &ActorContext,
        space_id: Uuid,
        id: Uuid,
    ) -> ApiResult<RegistrationView> {
        let space = self.host_space(ctx, space_id).await?;
        let r = self.scoped(space.id, id).await?;
        if r.status == "CANCELLED" {
            return Ok(RegistrationView::of(&r));
        }
        let txn = self.core.db.begin().await?;
        let now = Utc::now();
        let mut am: webinar_registrations::ActiveModel = r.clone().into();
        am.status = Set("CANCELLED".into());
        am.cancelled_at = Set(Some(now));
        am.cancelled_by = Set(crate::services::meeting_events::AuditActor::from_ctx(ctx).actor_ref);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE meeting_invitations SET status = 'REVOKED', revoked_at = now(), revoked_by = $2, updated_at = now()
              WHERE id = $1 AND status = 'ACTIVE'",
            [r.invitation_id.into(), ctx.user_id().into()],
        ))
        .await?;
        audit(
            &txn,
            &crate::services::meeting_events::AuditActor::from_ctx(ctx),
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: r.conference_id,
                action: action::REGISTRATION_CANCEL,
                target_type: Some("REGISTRATION"),
                target_ref: Some(r.id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(RegistrationView::of(&updated))
    }
}

// ---------------------------------------------------------------------------
// Backstage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackstageResponse {
    pub participant_id: Uuid,
    pub in_backstage: bool,
    pub media_context: MediaContextView,
    /// Request a fresh credential for the new context
    /// (`POST /api/v2/conferences/{id}/media-credential`).
    pub credential_refresh_required: bool,
}

pub struct BackstageService {
    core: Core,
}

impl BackstageService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    pub async fn enter(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<BackstageResponse> {
        self.switch(ctx, conference_id, true).await
    }

    pub async fn leave(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<BackstageResponse> {
        self.switch(ctx, conference_id, false).await
    }

    async fn switch(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        enter: bool,
    ) -> ApiResult<BackstageResponse> {
        if !self.core.flags.webinar_mode_enabled {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Webinar mode is not enabled",
            ));
        }
        let c = ArtifactCtx::load(&self.core, ctx, conference_id).await?;
        let me = c.me()?.clone();
        if enter {
            if !c.allows(Permission::BackstageEnter) {
                return Err(ApiError::domain(
                    ErrorCode::BackstageAccessDenied,
                    "Only hosts and panelists can enter the backstage",
                ));
            }
            if !c.profile.policy.stage.backstage_enabled {
                return Err(profile_denied("This meeting has no backstage"));
            }
        }
        if c.conference.status != ConferenceStatus::Live {
            return Err(ApiError::domain(
                ErrorCode::ConferenceNotLive,
                "The conference is not live",
            ));
        }
        // The provider room exists before anyone is routed into it: a provider failure
        // leaves the participant's context unchanged.
        if enter && RtcControl::new(self.core.clone()).room_control() {
            let room = RtcRoomRef {
                conference_id,
                room_name: backstage_room_name(conference_id),
            };
            if !matches!(self.core.rtc.get_room(&room).await, Ok(Some(_))) {
                self.core
                    .rtc
                    .create_room(&room, &RoomSpec::default())
                    .await?;
            }
        }
        let txn = self.core.db.begin().await?;
        let mut conference = lock_conference(&txn, conference_id).await?;
        if enter && conference.backstage_room_name.is_none() {
            let name = backstage_room_name(conference.id);
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE conference_sessions SET backstage_room_name = $2 WHERE id = $1 AND backstage_room_name IS NULL",
                [conference.id.into(), name.clone().into()],
            ))
            .await?;
            conference.backstage_room_name = Some(name);
        }
        let participant = conference_participants::Entity::find_by_id(me.id)
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .lock(LockType::Update)
            .one(&txn)
            .await?
            .ok_or_else(crate::services::meeting_core::not_found_participant)?;
        // Re-check under the participant lock: a concurrent demotion / removal (which
        // locks the same rows) either commits first and blocks entry here, or commits
        // after and evicts this participant from the backstage.
        if enter
            && !(participant.admission_state == AdmissionState::Admitted
                && c.pctx.allows(participant.role, Permission::BackstageEnter))
        {
            return Err(ApiError::domain(
                ErrorCode::BackstageAccessDenied,
                "Only hosts and panelists can enter the backstage",
            ));
        }
        let currently = participant.backstage_at.is_some();
        if currently == enter {
            txn.commit().await?;
            let context = crate::services::collaboration::breakouts::media_context(
                &self.core.db,
                &conference,
                &participant,
            )
            .await?;
            return Ok(BackstageResponse {
                participant_id: participant.id,
                in_backstage: currently,
                media_context: MediaContextView::from(&context),
                credential_refresh_required: false,
            });
        }
        let previous = crate::services::collaboration::breakouts::media_context(
            &txn,
            &conference,
            &participant,
        )
        .await?;
        let mut am: conference_participants::ActiveModel = participant.clone().into();
        am.backstage_at = Set(enter.then(Utc::now));
        am.collab_version = Set(participant.collab_version + 1);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;
        record_backstage_change(&txn, &conference, &updated, &previous, enter, &c.audit).await?;
        txn.commit().await?;
        if enter {
            inc(&OM.backstage_entries);
        } else {
            inc(&OM.backstage_exits);
        }
        let control = RtcControl::new(self.core.clone());
        if enter && control.room_control() {
            ensure_backstage_room(
                &self.core,
                &find_conference(&self.core.db, conference_id).await?,
            )
            .await?;
        }
        control.dispatch_for_conference(conference_id).await;
        let conference = find_conference(&self.core.db, conference_id).await?;
        let context = crate::services::collaboration::breakouts::media_context(
            &self.core.db,
            &conference,
            &updated,
        )
        .await?;
        Ok(BackstageResponse {
            participant_id: updated.id,
            in_backstage: enter,
            media_context: MediaContextView::from(&context),
            credential_refresh_required: true,
        })
    }
}

/// Audit + realtime + provider reroute for a backstage context change (on the locked
/// conference transaction). Used by enter / leave and by demotion / removal.
pub async fn record_backstage_change<C: ConnectionTrait>(
    txn: &C,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
    previous: &MediaContext,
    entered: bool,
    actor: &crate::services::meeting_events::AuditActor,
) -> ApiResult<()> {
    audit(
        txn,
        actor,
        AuditRecord {
            meeting_space_id: conference.meeting_space_id,
            conference_id: Some(conference.id),
            action: if entered {
                action::BACKSTAGE_ENTER
            } else {
                action::BACKSTAGE_LEAVE
            },
            target_type: Some("PARTICIPANT"),
            target_ref: Some(participant.id.to_string()),
            metadata: json!({}),
        },
    )
    .await?;
    emit_to(
        txn,
        OutboxRecord {
            aggregate_type: "conference_participant",
            aggregate_id: participant.id,
            event_type: if entered {
                event::BACKSTAGE_PARTICIPANT_JOINED
            } else {
                event::BACKSTAGE_PARTICIPANT_LEFT
            },
            meeting_space_id: Some(conference.meeting_space_id),
            conference_id: Some(conference.id),
            payload: json!({ "conferenceId": conference.id, "participantId": participant.id }),
        },
        // Backstage membership is visible to backstage-capable roles and the mover.
        Some(Audience::Participants {
            ids: vec![participant.id],
            moderator_permission: Some(Permission::BackstageEnter.key().into()),
        }),
    )
    .await?;
    // Remove provider sessions from the room the participant left (single media
    // context; the command re-reads authoritative state).
    emit(
        txn,
        OutboxRecord {
            aggregate_type: "conference_participant",
            aggregate_id: participant.id,
            event_type: event::RTC_PARTICIPANT_REROUTE_REQUESTED,
            meeting_space_id: Some(conference.meeting_space_id),
            conference_id: Some(conference.id),
            payload: json!({
                "conferenceId": conference.id,
                "participantId": participant.id,
                "fromBreakoutId": previous.breakout_id(),
                "fromBackstage": matches!(previous, MediaContext::Backstage { .. }),
            }),
        },
    )
    .await?;
    Ok(())
}

/// Demotion / removal: leave the backstage when the role no longer allows it.
pub async fn clear_backstage_if_unauthorized<C: ConnectionTrait>(
    txn: &C,
    conference: &conference_sessions::Model,
    participant: &conference_participants::Model,
    pctx: &PermissionContext,
    actor: &crate::services::meeting_events::AuditActor,
) -> ApiResult<Option<conference_participants::Model>> {
    if participant.backstage_at.is_none()
        || (participant.admission_state == AdmissionState::Admitted
            && pctx.allows(participant.role, Permission::BackstageEnter))
    {
        return Ok(None);
    }
    let previous = MediaContext::Backstage {
        room_name: conference.backstage_room_name.clone().unwrap_or_default(),
    };
    let mut am: conference_participants::ActiveModel = participant.clone().into();
    am.backstage_at = Set(None);
    am.collab_version = Set(participant.collab_version + 1);
    am.updated_at = Set(Utc::now());
    let updated = am.update(txn).await?;
    record_backstage_change(txn, conference, &updated, &previous, false, actor).await?;
    inc(&OM.backstage_evictions);
    Ok(Some(updated))
}
