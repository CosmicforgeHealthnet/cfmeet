//! MeetingSpaceService — create / read / update / archive / duplicate / owner transfer
//! and instant-meeting orchestration (spec §C1–§C8).

use chrono::Utc;
use chrono_tz::Tz;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{
        AccessPolicy, ConferenceKind, ConferenceStatus, MediaCapturePolicy, MeetingType,
        MemberStatus, PrincipalType, RecordOrigin, Role, SecurityProfile, SpaceStatus,
        WaitingRoomPolicy,
    },
    dto::v2::{
        AccessConfig, CreateMeetingSpaceRequest, DuplicateMeetingSpaceRequest,
        InstantMeetingRequest, OwnerTransferRequest, SchedulePolicy, UpdateMeetingSpaceRequest,
    },
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_sessions, meeting_members, meeting_spaces, users},
    services::{
        actor::{ActorContext, RegisteredActor},
        collaboration::policy::CollaborationPolicyPatch,
        conference::{insert_conference, NewConference},
        meeting_core::{
            find_space, is_unique_violation, lock_space, permission_denied, space_role,
            state_conflict, validation, Core,
        },
        meeting_events::{self, action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::{space_allows, PermissionContext, RolePolicy, SpacePermission},
        recurrence, secrets,
    },
};

/// Effective configuration, validated as a whole after create/patch.
#[derive(Debug, Clone)]
pub struct SpaceDraft {
    pub title: String,
    pub description: Option<String>,
    pub meeting_type: MeetingType,
    pub timezone: String,
    pub access_policy: AccessPolicy,
    pub waiting_room_policy: WaitingRoomPolicy,
    pub security_profile: SecurityProfile,
    pub default_role: Role,
    pub has_passcode: bool,
    pub interactive_capacity: i32,
    pub viewer_capacity: i32,
    pub recording_policy: MediaCapturePolicy,
    pub transcription_policy: MediaCapturePolicy,
    pub retention_policy_ref: Option<String>,
    pub organization_id: Option<Uuid>,
    pub access_config: AccessConfig,
    pub role_policy: RolePolicy,
    pub schedule_policy: SchedulePolicy,
    /// Part F: validated sparse collaboration overrides.
    pub collaboration_policy: CollaborationPolicyPatch,
}

fn valid_domain(d: &str) -> bool {
    d.len() <= 253
        && d.contains('.')
        && !d.starts_with('.')
        && d.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
}

pub fn validate_passcode(passcode: &str) -> ApiResult<()> {
    let len = passcode.chars().count();
    if !(4..=64).contains(&len) {
        return Err(validation("Passcode must be between 4 and 64 characters"));
    }
    Ok(())
}

impl SpaceDraft {
    /// Validate the complete configuration against business rules and rollout flags.
    pub fn validate(&self, flags: &crate::config::FeatureFlags) -> ApiResult<()> {
        let title_len = self.title.trim().chars().count();
        if title_len == 0 || title_len > 255 {
            return Err(validation("Title must be between 1 and 255 characters"));
        }
        if self
            .description
            .as_ref()
            .map(|d| d.chars().count() > 5000)
            .unwrap_or(false)
        {
            return Err(validation("Description must be at most 5000 characters"));
        }
        if self.timezone.parse::<Tz>().is_err() {
            return Err(validation(
                "timezone must be a valid IANA timezone identifier",
            ));
        }
        if !matches!(
            self.default_role,
            Role::Presenter | Role::Participant | Role::Viewer
        ) {
            return Err(validation(
                "defaultRole must be PRESENTER, PARTICIPANT or VIEWER",
            ));
        }
        if !(1..=1000).contains(&self.interactive_capacity) {
            return Err(validation("interactiveCapacity must be between 1 and 1000"));
        }
        if !(0..=100_000).contains(&self.viewer_capacity) {
            return Err(validation("viewerCapacity must be between 0 and 100000"));
        }
        if self
            .retention_policy_ref
            .as_ref()
            .map(|r| r.len() > 128)
            .unwrap_or(false)
        {
            return Err(validation("retentionPolicyRef is too long"));
        }
        if self.access_policy == AccessPolicy::PublicPasscode && !self.has_passcode {
            return Err(ApiError::domain(
                ErrorCode::MeetingPasscodeRequired,
                "PUBLIC_PASSCODE access requires a passcode",
            ));
        }
        if self.access_policy == AccessPolicy::DomainRestricted
            && self.access_config.allowed_domains.is_empty()
        {
            return Err(validation(
                "DOMAIN_RESTRICTED access requires accessConfig.allowedDomains",
            ));
        }
        if !self
            .access_config
            .allowed_domains
            .iter()
            .all(|d| valid_domain(d))
        {
            return Err(validation("allowedDomains must be lowercase domain names"));
        }
        if self.access_policy == AccessPolicy::Custom
            && self
                .access_config
                .adapter_ref
                .as_deref()
                .unwrap_or("")
                .is_empty()
        {
            return Err(validation("CUSTOM access requires accessConfig.adapterRef"));
        }
        if self.access_policy == AccessPolicy::OrganizationOnly && self.organization_id.is_none() {
            return Err(validation(
                "ORGANIZATION_ONLY access requires organizationId",
            ));
        }
        if self.waiting_room_policy == WaitingRoomPolicy::Custom
            && self
                .access_config
                .waiting_room_adapter_ref
                .as_deref()
                .unwrap_or("")
                .is_empty()
        {
            return Err(validation(
                "CUSTOM waiting room requires accessConfig.waitingRoomAdapterRef",
            ));
        }
        if self.waiting_room_policy == WaitingRoomPolicy::ExternalOnly
            && self.organization_id.is_none()
            && self.access_config.allowed_domains.is_empty()
        {
            return Err(validation(
                "EXTERNAL_ONLY waiting room requires organizationId or accessConfig.allowedDomains",
            ));
        }
        if self.waiting_room_policy != WaitingRoomPolicy::Off && !flags.waiting_room_v2_enabled {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Waiting room v2 is not enabled",
            ));
        }
        let healthcare = self.meeting_type == MeetingType::Healthcare
            || self.security_profile == SecurityProfile::Healthcare
            || self.access_policy == AccessPolicy::HealthcareAppointment;
        if healthcare && !flags.healthcare_profile_enabled {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Healthcare profile is not enabled",
            ));
        }
        if matches!(
            self.meeting_type,
            MeetingType::Webinar | MeetingType::TownHall
        ) && !flags.webinar_mode_enabled
        {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Webinar mode is not enabled",
            ));
        }
        self.role_policy.validate().map_err(validation)?;
        self.collaboration_policy.validate().map_err(validation)?;
        let s = &self.schedule_policy;
        if s.early_start_minutes > 240
            || s.late_start_grace_minutes > 1440
            || s.auto_end_grace_minutes > 1440
        {
            return Err(validation("schedulePolicy values are out of range"));
        }
        Ok(())
    }

    fn from_create(req: &CreateMeetingSpaceRequest) -> Self {
        Self {
            title: req.title.trim().to_string(),
            description: req.description.clone().filter(|d| !d.trim().is_empty()),
            meeting_type: req.meeting_type.unwrap_or(MeetingType::Standard),
            timezone: req.timezone.clone().unwrap_or_else(|| "UTC".into()),
            access_policy: req.access_policy.unwrap_or(AccessPolicy::Authenticated),
            waiting_room_policy: req.waiting_room_policy.unwrap_or(WaitingRoomPolicy::Off),
            security_profile: req.security_profile.unwrap_or(SecurityProfile::Standard),
            // Part F §53: webinar attendees are subscribe-only unless explicitly configured.
            default_role: req.default_role.unwrap_or(
                if matches!(
                    req.meeting_type,
                    Some(MeetingType::Webinar | MeetingType::TownHall)
                ) {
                    Role::Viewer
                } else {
                    Role::Participant
                },
            ),
            has_passcode: req.passcode.is_some(),
            interactive_capacity: req.interactive_capacity.unwrap_or(100),
            viewer_capacity: req.viewer_capacity.unwrap_or(0),
            recording_policy: req
                .recording_policy
                .unwrap_or(MediaCapturePolicy::HostOptional),
            transcription_policy: req
                .transcription_policy
                .unwrap_or(MediaCapturePolicy::Disabled),
            retention_policy_ref: req.retention_policy_ref.clone(),
            organization_id: req.organization_id,
            access_config: req.access_config.clone().unwrap_or_default(),
            role_policy: req.role_policy.clone().unwrap_or_default(),
            schedule_policy: req.schedule_policy.unwrap_or_default(),
            collaboration_policy: req.collaboration_policy.clone().unwrap_or_default(),
        }
    }

    pub fn from_model(m: &meeting_spaces::Model) -> Self {
        Self {
            title: m.title.clone(),
            description: m.description.clone(),
            meeting_type: m.meeting_type,
            timezone: m.timezone.clone(),
            access_policy: m.access_policy,
            waiting_room_policy: m.waiting_room_policy,
            security_profile: m.security_profile,
            default_role: m.default_role,
            has_passcode: m.passcode_hash.is_some(),
            interactive_capacity: m.interactive_capacity,
            viewer_capacity: m.viewer_capacity,
            recording_policy: m.recording_policy,
            transcription_policy: m.transcription_policy,
            retention_policy_ref: m.retention_policy_ref.clone(),
            organization_id: m.organization_id,
            access_config: access_config(m),
            role_policy: RolePolicy::from_json(&m.role_policy),
            schedule_policy: schedule_policy(m),
            collaboration_policy: CollaborationPolicyPatch::from_json(&m.collaboration_policy),
        }
    }
}

pub fn access_config(m: &meeting_spaces::Model) -> AccessConfig {
    serde_json::from_value(m.access_config.clone()).unwrap_or_default()
}

pub fn schedule_policy(m: &meeting_spaces::Model) -> SchedulePolicy {
    serde_json::from_value(m.schedule_policy.clone()).unwrap_or_default()
}

/// What the caller may see of a space.
#[derive(Debug, Clone)]
pub struct SpaceView {
    pub space: meeting_spaces::Model,
    pub viewer_role: Option<Role>,
    pub full: bool,
}

pub struct MeetingSpaceService {
    core: Core,
}

impl MeetingSpaceService {
    pub fn new(core: Core) -> Self {
        Self { core }
    }

    async fn verify_organization(&self, actor: &ActorContext, org: Option<Uuid>) -> ApiResult<()> {
        if let Some(org) = org {
            if !actor.verified_organizations.contains(&org) {
                return Err(permission_denied(
                    "You are not a verified member of that organization",
                ));
            }
        }
        Ok(())
    }

    /// Insert a space with a fresh unique meeting code (savepoint-protected retries).
    async fn insert_space<C: ConnectionTrait + TransactionTrait>(
        &self,
        conn: &C,
        owner: Uuid,
        draft: &SpaceDraft,
        passcode_hash: Option<String>,
    ) -> ApiResult<meeting_spaces::Model> {
        let now = Utc::now();
        for _ in 0..5 {
            let model = meeting_spaces::ActiveModel {
                id: Set(Uuid::new_v4()),
                legacy_meeting_id: Set(None),
                organization_id: Set(draft.organization_id),
                owner_user_id: Set(owner),
                meeting_code: Set(secrets::generate_meeting_code()),
                title: Set(draft.title.trim().to_string()),
                description: Set(draft.description.clone()),
                meeting_type: Set(draft.meeting_type),
                status: Set(SpaceStatus::Active),
                timezone: Set(draft.timezone.clone()),
                access_policy: Set(draft.access_policy),
                waiting_room_policy: Set(draft.waiting_room_policy),
                security_profile: Set(draft.security_profile),
                default_role: Set(draft.default_role),
                passcode_hash: Set(passcode_hash.clone()),
                interactive_capacity: Set(draft.interactive_capacity),
                viewer_capacity: Set(draft.viewer_capacity),
                recording_policy: Set(draft.recording_policy),
                transcription_policy: Set(draft.transcription_policy),
                retention_policy_ref: Set(draft.retention_policy_ref.clone()),
                access_config: Set(
                    serde_json::to_value(&draft.access_config).unwrap_or_else(|_| json!({}))
                ),
                role_policy: Set(
                    serde_json::to_value(&draft.role_policy).unwrap_or_else(|_| json!({}))
                ),
                schedule_policy: Set(
                    serde_json::to_value(draft.schedule_policy).unwrap_or_else(|_| json!({}))
                ),
                origin: Set(RecordOrigin::V2),
                created_at: Set(now),
                updated_at: Set(now),
                archived_at: Set(None),
                version: Set(1),
                collaboration_policy: Set(draft.collaboration_policy.to_json()),
                profile_policy: Set(serde_json::json!({})),
                profile_policy_version: Set(1),
            };
            let savepoint = conn.begin().await?;
            match model.insert(&savepoint).await {
                Ok(m) => {
                    savepoint.commit().await?;
                    return Ok(m);
                }
                Err(e) if is_unique_violation(&e) => {
                    savepoint.rollback().await?;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(ApiError::InternalError(
            "could not allocate a unique meeting code".into(),
        ))
    }

    async fn record_created<C: ConnectionTrait>(
        &self,
        conn: &C,
        actor: &AuditActor,
        space: &meeting_spaces::Model,
        extra: serde_json::Value,
    ) -> ApiResult<()> {
        audit(
            conn,
            actor,
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: None,
                action: action::SPACE_CREATE,
                target_type: Some("MEETING_SPACE"),
                target_ref: Some(space.id.to_string()),
                metadata: json!({
                    "accessPolicy": space.access_policy,
                    "waitingRoomPolicy": space.waiting_room_policy,
                    "securityProfile": space.security_profile,
                    "hasPasscode": space.passcode_hash.is_some(),
                    "extra": extra,
                }),
            },
        )
        .await?;
        emit(
            conn,
            OutboxRecord {
                aggregate_type: "meeting_space",
                aggregate_id: space.id,
                event_type: event::SPACE_CREATED,
                meeting_space_id: Some(space.id),
                conference_id: None,
                payload: json!({ "meetingSpaceId": space.id, "meetingCode": space.meeting_code }),
            },
        )
        .await
    }

    /// `POST /api/v2/meeting-spaces` — owner always comes from the trusted actor.
    pub async fn create(
        &self,
        ctx: &ActorContext,
        req: CreateMeetingSpaceRequest,
    ) -> ApiResult<meeting_spaces::Model> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        let draft = SpaceDraft::from_create(&req);
        if let Some(p) = &req.passcode {
            validate_passcode(p)?;
        }
        draft.validate(&self.core.flags)?;
        self.verify_organization(ctx, draft.organization_id).await?;
        let passcode_hash = match &req.passcode {
            Some(p) => Some(secrets::hash_passcode(
                p,
                self.core.config.passcode_hash_cost,
            )?),
            None => None,
        };
        let txn = self.core.db.begin().await?;
        let space = self
            .insert_space(&txn, actor.user_id, &draft, passcode_hash)
            .await?;
        self.record_created(&txn, &AuditActor::from(ctx), &space, json!({}))
            .await?;
        txn.commit().await?;
        Ok(space)
    }

    /// Instant meeting: MeetingSpace + LIVE ConferenceSession in one transaction.
    pub async fn create_instant(
        &self,
        ctx: &ActorContext,
        req: InstantMeetingRequest,
    ) -> ApiResult<(meeting_spaces::Model, conference_sessions::Model)> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        let create = CreateMeetingSpaceRequest {
            title: req
                .title
                .clone()
                .unwrap_or_else(|| "Instant meeting".into()),
            timezone: req.timezone.clone(),
            access_policy: Some(req.access_policy.unwrap_or(AccessPolicy::Public)),
            waiting_room_policy: req.waiting_room_policy,
            passcode: req.passcode.clone(),
            ..Default::default()
        };
        let draft = SpaceDraft::from_create(&create);
        if let Some(p) = &create.passcode {
            validate_passcode(p)?;
        }
        draft.validate(&self.core.flags)?;
        let passcode_hash = match &create.passcode {
            Some(p) => Some(secrets::hash_passcode(
                p,
                self.core.config.passcode_hash_cost,
            )?),
            None => None,
        };
        let audit_actor = AuditActor::from(ctx);
        let txn = self.core.db.begin().await?;
        let space = self
            .insert_space(&txn, actor.user_id, &draft, passcode_hash)
            .await?;
        self.record_created(&txn, &audit_actor, &space, json!({ "instant": true }))
            .await?;
        let now = Utc::now();
        let conference = insert_conference(
            &txn,
            &space,
            NewConference {
                kind: ConferenceKind::Instant,
                occurrence_key: format!("instant:{}", Uuid::new_v4()),
                scheduled_start_at: Some(now),
                scheduled_end_at: None,
                recurrence_rule_id: None,
                status: ConferenceStatus::Live,
                started_by: Some(audit_actor.reference()),
            },
            &audit_actor,
        )
        .await?;
        txn.commit().await?;
        // Part E: an instant conference is LIVE immediately; ensure its provider room now
        // (best effort — the first join and the reconciler retry, and no credential is
        // ever issued before the room is confirmed).
        let control = crate::services::rtc_control::RtcControl::new(self.core.clone());
        let conference = if control.room_control() {
            match control.ensure_room(conference.id).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(conference_id = %conference.id, "instant meeting room not ready yet: {e}");
                    conference
                }
            }
        } else {
            conference
        };
        Ok((space, conference))
    }

    /// Persistent role of the actor in a space (owner / member).
    pub async fn viewer_role<C: ConnectionTrait>(
        &self,
        conn: &C,
        space: &meeting_spaces::Model,
        ctx: &ActorContext,
    ) -> ApiResult<Option<Role>> {
        Ok(match ctx.registered() {
            Some(r) => space_role(conn, space, r).await?.map(|(role, _)| role),
            None => None,
        })
    }

    pub async fn view_of(
        &self,
        ctx: &ActorContext,
        space: meeting_spaces::Model,
    ) -> ApiResult<SpaceView> {
        // An unverifiable session on a strict space only gets the summary view.
        let viewer_role = match self.viewer_role(&self.core.db, &space, ctx).await {
            Err(e) if e.code() == Some(ErrorCode::RevocationCheckUnavailable) => None,
            other => other?,
        };
        let pctx = PermissionContext::for_space(&space);
        let full = viewer_role
            .map(|r| space_allows(r, SpacePermission::ViewMembers, &pctx))
            .unwrap_or(false);
        Ok(SpaceView {
            space,
            viewer_role,
            full,
        })
    }

    /// Spaces the actor owns or is an active member of.
    pub async fn list_for_actor(&self, ctx: &ActorContext) -> ApiResult<Vec<SpaceView>> {
        let actor = ctx.require_registered()?;
        let member_space_ids: Vec<Uuid> = meeting_members::Entity::find()
            .filter(meeting_members::Column::UserId.eq(actor.user_id))
            .filter(meeting_members::Column::Status.eq(MemberStatus::Active))
            .all(&self.core.db)
            .await?
            .into_iter()
            .map(|m| m.meeting_space_id)
            .collect();
        let spaces = meeting_spaces::Entity::find()
            .filter(
                sea_orm::Condition::any()
                    .add(meeting_spaces::Column::OwnerUserId.eq(actor.user_id))
                    .add(meeting_spaces::Column::Id.is_in(member_space_ids)),
            )
            .filter(meeting_spaces::Column::Status.ne(SpaceStatus::Deleted))
            .order_by_desc(meeting_spaces::Column::CreatedAt)
            .all(&self.core.db)
            .await?;
        let mut out = Vec::with_capacity(spaces.len());
        for s in spaces
            .into_iter()
            .filter(crate::services::meeting_core::space_is_visible)
        {
            out.push(self.view_of(ctx, s).await?);
        }
        Ok(out)
    }

    async fn require_space_permission<C: ConnectionTrait>(
        &self,
        conn: &C,
        space: &meeting_spaces::Model,
        actor: &RegisteredActor,
        permission: SpacePermission,
    ) -> ApiResult<Role> {
        let role = space_role(conn, space, actor).await?.map(|(r, _)| r);
        let pctx = PermissionContext::for_space(space);
        match role {
            Some(r) if space_allows(r, permission, &pctx) => Ok(r),
            _ => Err(permission_denied(
                "You are not permitted to manage this meeting",
            )),
        }
    }

    /// `PATCH /api/v2/meeting-spaces/{id}` — authorized, versioned, audited.
    pub async fn update(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        req: UpdateMeetingSpaceRequest,
    ) -> ApiResult<meeting_spaces::Model> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        if let Some(p) = &req.passcode {
            validate_passcode(p)?;
        }
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, id).await?;
        self.require_space_permission(&txn, &space, actor, SpacePermission::Configure)
            .await?;
        if space.version != req.version {
            return Err(ApiError::domain(
                ErrorCode::MeetingVersionConflict,
                format!("Meeting was modified (current version {})", space.version),
            ));
        }
        if space.status != SpaceStatus::Active {
            return Err(state_conflict("Archived meeting spaces cannot be modified"));
        }

        let before = SpaceDraft::from_model(&space);
        let mut draft = before.clone();
        if let Some(v) = &req.title {
            draft.title = v.trim().to_string();
        }
        if let Some(v) = &req.description {
            draft.description = Some(v.clone()).filter(|d| !d.trim().is_empty());
        }
        if let Some(v) = req.meeting_type {
            draft.meeting_type = v;
        }
        if let Some(v) = &req.timezone {
            draft.timezone = v.clone();
        }
        if let Some(v) = req.access_policy {
            draft.access_policy = v;
        }
        if let Some(v) = req.waiting_room_policy {
            draft.waiting_room_policy = v;
        }
        if let Some(v) = req.security_profile {
            draft.security_profile = v;
        }
        if let Some(v) = req.default_role {
            draft.default_role = v;
        }
        if let Some(v) = req.interactive_capacity {
            draft.interactive_capacity = v;
        }
        if let Some(v) = req.viewer_capacity {
            draft.viewer_capacity = v;
        }
        if let Some(v) = req.recording_policy {
            draft.recording_policy = v;
        }
        if let Some(v) = req.transcription_policy {
            draft.transcription_policy = v;
        }
        if let Some(v) = &req.retention_policy_ref {
            draft.retention_policy_ref = Some(v.clone()).filter(|s| !s.is_empty());
        }
        if let Some(v) = &req.access_config {
            draft.access_config = v.clone();
        }
        if let Some(v) = &req.role_policy {
            draft.role_policy = v.clone();
        }
        if let Some(v) = req.schedule_policy {
            draft.schedule_policy = v;
        }
        if let Some(v) = &req.collaboration_policy {
            draft.collaboration_policy = v.clone();
        }
        let passcode_changed = req.passcode.is_some() || req.clear_passcode;
        if req.clear_passcode {
            draft.has_passcode = false;
        }
        if req.passcode.is_some() {
            draft.has_passcode = true;
        }
        draft.validate(&self.core.flags)?;

        let mut sensitive: Vec<&str> = vec![];
        if before.access_policy != draft.access_policy {
            sensitive.push("accessPolicy");
        }
        if before.waiting_room_policy != draft.waiting_room_policy {
            sensitive.push("waitingRoomPolicy");
        }
        if before.security_profile != draft.security_profile {
            sensitive.push("securityProfile");
        }
        if before.role_policy != draft.role_policy {
            sensitive.push("rolePolicy");
        }
        if before.collaboration_policy != draft.collaboration_policy {
            sensitive.push("collaborationPolicy");
        }
        if before.access_config != draft.access_config {
            sensitive.push("accessConfig");
        }
        if before.recording_policy != draft.recording_policy
            || before.transcription_policy != draft.transcription_policy
        {
            sensitive.push("capturePolicy");
        }
        if passcode_changed {
            sensitive.push("passcode");
        }
        let timezone_changed = before.timezone != draft.timezone;

        let new_hash = match (&req.passcode, req.clear_passcode) {
            (Some(p), _) => Some(Some(secrets::hash_passcode(
                p,
                self.core.config.passcode_hash_cost,
            )?)),
            (None, true) => Some(None),
            _ => None,
        };
        let new_version = space.version + 1;
        let mut am: meeting_spaces::ActiveModel = space.clone().into();
        am.title = Set(draft.title.clone());
        am.description = Set(draft.description.clone());
        am.meeting_type = Set(draft.meeting_type);
        am.timezone = Set(draft.timezone.clone());
        am.access_policy = Set(draft.access_policy);
        am.waiting_room_policy = Set(draft.waiting_room_policy);
        am.security_profile = Set(draft.security_profile);
        am.default_role = Set(draft.default_role);
        am.interactive_capacity = Set(draft.interactive_capacity);
        am.viewer_capacity = Set(draft.viewer_capacity);
        am.recording_policy = Set(draft.recording_policy);
        am.collaboration_policy = Set(draft.collaboration_policy.to_json());
        am.transcription_policy = Set(draft.transcription_policy);
        am.retention_policy_ref = Set(draft.retention_policy_ref.clone());
        am.access_config =
            Set(serde_json::to_value(&draft.access_config).unwrap_or_else(|_| json!({})));
        am.role_policy =
            Set(serde_json::to_value(&draft.role_policy).unwrap_or_else(|_| json!({})));
        am.schedule_policy =
            Set(serde_json::to_value(draft.schedule_policy).unwrap_or_else(|_| json!({})));
        if let Some(h) = new_hash {
            am.passcode_hash = Set(h);
        }
        am.version = Set(new_version);
        am.updated_at = Set(Utc::now());
        let updated = am.update(&txn).await?;

        if timezone_changed {
            recurrence::follow_space_timezone(&txn, &updated, &self.core.config).await?;
        }

        let audit_actor = AuditActor::from(ctx);
        audit(
            &txn,
            &audit_actor,
            AuditRecord {
                meeting_space_id: id,
                conference_id: None,
                action: if sensitive.is_empty() {
                    action::SPACE_UPDATE
                } else {
                    action::SPACE_POLICY_UPDATE
                },
                target_type: Some("MEETING_SPACE"),
                target_ref: Some(id.to_string()),
                metadata: json!({
                    "fromVersion": space.version,
                    "toVersion": new_version,
                    "sensitiveFields": sensitive,
                    "accessPolicy": updated.access_policy,
                    "waitingRoomPolicy": updated.waiting_room_policy,
                }),
            },
        )
        .await?;
        emit(
            &txn,
            OutboxRecord {
                aggregate_type: "meeting_space",
                aggregate_id: id,
                event_type: event::SPACE_UPDATED,
                meeting_space_id: Some(id),
                conference_id: None,
                payload: json!({ "meetingSpaceId": id, "version": new_version }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }

    /// Soft archive. History, conferences, participants, audit and legacy mapping are kept.
    pub async fn archive(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        version: Option<i32>,
    ) -> ApiResult<meeting_spaces::Model> {
        let actor = ctx.require_registered()?;
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, id).await?;
        if !actor.platform_admin {
            self.require_space_permission(&txn, &space, actor, SpacePermission::Archive)
                .await?;
        }
        if let Some(v) = version {
            if v != space.version {
                return Err(ApiError::domain(
                    ErrorCode::MeetingVersionConflict,
                    "Meeting was modified",
                ));
            }
        }
        if space.status == SpaceStatus::Archived {
            txn.commit().await?;
            return Ok(space);
        }
        let now = Utc::now();
        let mut am: meeting_spaces::ActiveModel = space.clone().into();
        am.status = Set(SpaceStatus::Archived);
        am.archived_at = Set(Some(now));
        am.version = Set(space.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        // No new occurrences: end the series and cancel unstarted future occurrences.
        let cancelled = recurrence::stop_series_for_archive(&txn, id, now).await?;
        let audit_actor = AuditActor::from(ctx);
        audit(
            &txn,
            &audit_actor,
            AuditRecord {
                meeting_space_id: id,
                conference_id: None,
                action: action::SPACE_ARCHIVE,
                target_type: Some("MEETING_SPACE"),
                target_ref: Some(id.to_string()),
                metadata: json!({ "cancelledFutureOccurrences": cancelled }),
            },
        )
        .await?;
        emit(
            &txn,
            OutboxRecord {
                aggregate_type: "meeting_space",
                aggregate_id: id,
                event_type: event::SPACE_ARCHIVED,
                meeting_space_id: Some(id),
                conference_id: None,
                payload: json!({ "meetingSpaceId": id }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }

    /// Duplicate reusable configuration only (no history, invitations, artefacts or rooms).
    pub async fn duplicate(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        req: DuplicateMeetingSpaceRequest,
    ) -> ApiResult<meeting_spaces::Model> {
        let actor = ctx.require_registered()?;
        self.core.require_v2_write(actor)?;
        let source = find_space(&self.core.db, id).await?;
        self.require_space_permission(&self.core.db, &source, actor, SpacePermission::Duplicate)
            .await?;
        let mut draft = SpaceDraft::from_model(&source);
        draft.title = req.title.map(|t| t.trim().to_string()).unwrap_or_else(|| {
            format!("Copy of {}", source.title)
                .chars()
                .take(255)
                .collect()
        });
        if let Some(org) = draft.organization_id {
            if !ctx.verified_organizations.contains(&org) {
                draft.organization_id = None;
            }
        }
        draft.validate(&self.core.flags)?;
        let txn = self.core.db.begin().await?;
        let space = self
            .insert_space(&txn, actor.user_id, &draft, source.passcode_hash.clone())
            .await?;
        let audit_actor = AuditActor::from(ctx);
        self.record_created(
            &txn,
            &audit_actor,
            &space,
            json!({ "duplicatedFrom": source.id }),
        )
        .await?;
        audit(
            &txn,
            &audit_actor,
            AuditRecord {
                meeting_space_id: source.id,
                conference_id: None,
                action: action::SPACE_DUPLICATE,
                target_type: Some("MEETING_SPACE"),
                target_ref: Some(space.id.to_string()),
                metadata: json!({}),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(space)
    }

    /// Dedicated, protected ownership transfer (never through PATCH).
    pub async fn transfer_owner(
        &self,
        ctx: &ActorContext,
        id: Uuid,
        req: OwnerTransferRequest,
    ) -> ApiResult<meeting_spaces::Model> {
        let actor = ctx.require_registered()?;
        let txn = self.core.db.begin().await?;
        let space = lock_space(&txn, id).await?;
        if !actor.platform_admin {
            self.require_space_permission(&txn, &space, actor, SpacePermission::TransferOwnership)
                .await?;
        }
        if space.version != req.version {
            return Err(ApiError::domain(
                ErrorCode::MeetingVersionConflict,
                "Meeting was modified",
            ));
        }
        if space.status != SpaceStatus::Active {
            return Err(state_conflict(
                "Archived meeting spaces cannot change owner",
            ));
        }
        if req.new_owner_user_id == space.owner_user_id {
            return Err(validation("That user already owns this meeting"));
        }
        let target = users::Entity::find_by_id(req.new_owner_user_id)
            .one(&txn)
            .await?
            .filter(|u| u.status == users::UserStatus::Active && u.role != users::UserRole::Guest)
            .ok_or_else(|| validation("New owner must be an active registered user"))?;

        let now = Utc::now();
        let previous_owner = space.owner_user_id;
        // The new owner's membership is superseded by ownership.
        meeting_members::Entity::update_many()
            .col_expr(
                meeting_members::Column::Status,
                sea_orm::sea_query::Expr::value(MemberStatus::Revoked.as_str()),
            )
            .col_expr(
                meeting_members::Column::RevokedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .col_expr(
                meeting_members::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(meeting_members::Column::MeetingSpaceId.eq(id))
            .filter(meeting_members::Column::UserId.eq(target.id))
            .filter(meeting_members::Column::Status.eq(MemberStatus::Active))
            .exec(&txn)
            .await?;
        if req.retain_previous_owner {
            meeting_members::ActiveModel {
                id: Set(Uuid::new_v4()),
                meeting_space_id: Set(id),
                principal_type: Set(PrincipalType::User),
                principal_ref: Set(previous_owner.to_string()),
                user_id: Set(Some(previous_owner)),
                role: Set(Role::Host),
                status: Set(MemberStatus::Active),
                created_by: Set(Some(actor.user_id)),
                created_at: Set(now),
                updated_at: Set(now),
                revoked_at: Set(None),
                revoked_by: Set(None),
                version: Set(1),
            }
            .insert(&txn)
            .await?;
        }
        let mut am: meeting_spaces::ActiveModel = space.clone().into();
        am.owner_user_id = Set(target.id);
        am.version = Set(space.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        let audit_actor = AuditActor::from(ctx);
        audit(
            &txn,
            &audit_actor,
            AuditRecord {
                meeting_space_id: id,
                conference_id: None,
                action: action::SPACE_OWNER_TRANSFER,
                target_type: Some("USER"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({
                    "previousOwner": previous_owner,
                    "retainedPreviousOwnerAsHost": req.retain_previous_owner,
                }),
            },
        )
        .await?;
        meeting_events::emit(
            &txn,
            OutboxRecord {
                aggregate_type: "meeting_space",
                aggregate_id: id,
                event_type: event::SPACE_OWNER_TRANSFERRED,
                meeting_space_id: Some(id),
                conference_id: None,
                payload: json!({ "meetingSpaceId": id, "ownerUserId": target.id }),
            },
        )
        .await?;
        txn.commit().await?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FeatureFlags;

    fn draft() -> SpaceDraft {
        SpaceDraft::from_create(&CreateMeetingSpaceRequest {
            title: "Weekly".into(),
            ..Default::default()
        })
    }

    #[test]
    fn defaults_are_valid_and_safe() {
        let d = draft();
        assert_eq!(d.access_policy, AccessPolicy::Authenticated);
        assert_eq!(d.waiting_room_policy, WaitingRoomPolicy::Off);
        assert_eq!(d.default_role, Role::Participant);
        assert!(d.validate(&FeatureFlags::default()).is_ok());
    }

    #[test]
    fn validation_rules() {
        let flags = FeatureFlags::all_v2_enabled();
        let mut d = draft();
        d.title = "  ".into();
        assert!(d.validate(&flags).is_err());

        let mut d = draft();
        d.timezone = "Mars/Olympus".into();
        assert!(d.validate(&flags).is_err());

        let mut d = draft();
        d.default_role = Role::Host;
        assert!(
            d.validate(&flags).is_err(),
            "default role can never be privileged"
        );

        let mut d = draft();
        d.access_policy = AccessPolicy::PublicPasscode;
        assert_eq!(
            d.validate(&flags).unwrap_err().code(),
            Some(ErrorCode::MeetingPasscodeRequired)
        );
        d.has_passcode = true;
        assert!(d.validate(&flags).is_ok());

        let mut d = draft();
        d.access_policy = AccessPolicy::DomainRestricted;
        assert!(d.validate(&flags).is_err());
        d.access_config.allowed_domains = vec!["example.com".into()];
        assert!(d.validate(&flags).is_ok());
        d.access_config.allowed_domains = vec!["Bad@Domain".into()];
        assert!(d.validate(&flags).is_err());

        let mut d = draft();
        d.access_policy = AccessPolicy::OrganizationOnly;
        assert!(d.validate(&flags).is_err());

        let mut d = draft();
        d.access_policy = AccessPolicy::Custom;
        assert!(d.validate(&flags).is_err());
        d.access_config.adapter_ref = Some("acme".into());
        assert!(d.validate(&flags).is_ok());
    }

    #[test]
    fn rollout_flags_gate_configuration() {
        let off = FeatureFlags::default();
        let mut d = draft();
        d.waiting_room_policy = WaitingRoomPolicy::Everyone;
        assert_eq!(
            d.validate(&off).unwrap_err().code(),
            Some(ErrorCode::FeatureDisabled)
        );
        let mut d = draft();
        d.meeting_type = MeetingType::Healthcare;
        assert_eq!(
            d.validate(&off).unwrap_err().code(),
            Some(ErrorCode::FeatureDisabled)
        );
        assert!(d.validate(&FeatureFlags::all_v2_enabled()).is_ok());
    }

    #[test]
    fn passcode_length_bounds() {
        assert!(validate_passcode("123").is_err());
        assert!(validate_passcode("1234").is_ok());
        assert!(validate_passcode(&"x".repeat(65)).is_err());
    }
}
