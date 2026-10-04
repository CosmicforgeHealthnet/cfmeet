//! AdmissionService — join pipeline, waiting room, role change and moderation
//! business authority (spec §D1–§D22, §4, §6).
//!
//! Join ordering (spec §D4): resolve conference/space → actor → lifecycle → space
//! status → access policy (incl. membership/invitation/passcode) → lock → ban →
//! logical participant → role → waiting room → COMMIT → participant session → RTC
//! credentials (only after the authoritative state committed).
//!
//! Lock order everywhere: conference row → participant rows (by id) → invitation.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    sea_query::{Expr, LockType},
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseTransaction, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{
        AdmissionState, BanScope, BanSubjectType, ConferenceStatus, IdentityType,
        MediaCapturePolicy, RecordOrigin, Role, RoleSource, SessionState, SpaceStatus,
        WaitingRoomPolicy,
    },
    dto::v2::{BanParticipantRequest, DenyRequest, JoinConferenceRequest, RoleChangeRequest},
    error::{ApiError, ApiResult, ErrorCode},
    models::{
        conference_bans, conference_participants, conference_sessions, meeting_spaces,
        participant_sessions,
    },
    services::{
        actor::{ActorContext, ActorKind},
        invitations::{self, InvitationService},
        meeting_access::{AccessDecision, AccessRequest, MeetingAccessService},
        meeting_core::{
            conference_authority, find_conference, find_scoped_participant, find_space,
            lock_conference, not_found_participant, own_participant, permission_denied,
            state_conflict, validation, ConferenceAuthority, Core,
        },
        meeting_events::{action, audit, emit, event, AuditActor, AuditRecord, OutboxRecord},
        meeting_permissions::{
            can_moderate, check_role_change, MediaPermissions, Permission, PermissionContext,
        },
        meeting_space::access_config,
        rtc_control::{Enforcement, RtcControl},
        rtc_metrics::{inc, RTC_METRICS as M},
        rtc_provider::{JoinCredentialRequest, JoinCredentials, RtcParticipantRef},
        secrets,
    },
};

// ---------------------------------------------------------------------------
// Pure policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WaitingInput {
    pub policy: WaitingRoomPolicy,
    pub identity_type: IdentityType,
    /// Actor holds `participant.admit` (hosts never wait for themselves).
    pub exempt: bool,
    pub is_member: bool,
    /// Inside the owning organization / domain.
    pub is_internal: bool,
    /// CUSTOM evaluator result.
    pub custom: Option<bool>,
}

/// Waiting-room policy (spec §D2). Evaluated only after access was granted.
pub fn must_wait(i: &WaitingInput) -> bool {
    if i.exempt {
        return false;
    }
    match i.policy {
        WaitingRoomPolicy::Off => false,
        WaitingRoomPolicy::Everyone => true,
        WaitingRoomPolicy::GuestsOnly => i.identity_type == IdentityType::Guest,
        WaitingRoomPolicy::UnauthenticatedOnly => i.identity_type != IdentityType::RegisteredUser,
        WaitingRoomPolicy::NonMembersOnly => !i.is_member,
        WaitingRoomPolicy::ExternalOnly => !i.is_internal,
        // Fail safe: no evaluator decision → wait.
        WaitingRoomPolicy::Custom => i.custom.unwrap_or(true),
    }
}

/// Role from authoritative sources: owner > strongest of membership / invitation /
/// adapter > space default. Never from the request.
pub fn resolve_role(decision: &AccessDecision, default_role: Role) -> (Role, RoleSource) {
    if decision.is_owner {
        return (Role::Owner, RoleSource::Owner);
    }
    let mut best: Option<(Role, RoleSource)> = None;
    let mut consider = |role: Role, source: RoleSource| {
        if best.map(|(b, _)| role.rank() > b.rank()).unwrap_or(true) {
            best = Some((role, source));
        }
    };
    if let Some(m) = &decision.membership {
        consider(m.role, RoleSource::Membership);
    }
    if let Some(i) = &decision.invitation {
        consider(i.requested_role, RoleSource::Invitation);
    }
    if let Some(r) = decision.adapter_role {
        consider(r, RoleSource::Default);
    }
    best.unwrap_or((default_role, RoleSource::Default))
}

pub fn sanitize_display_name(raw: &str) -> ApiResult<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    let len = cleaned.chars().count();
    if len == 0 || len > 100 {
        return Err(validation(
            "displayName must be between 1 and 100 characters",
        ));
    }
    Ok(cleaned.to_string())
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum RtcOutcome {
    Issued(JoinCredentials),
    NotIssuedWaiting,
    ProviderControlDisabled,
}

#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub conference: conference_sessions::Model,
    pub participant: conference_participants::Model,
    pub session: Option<participant_sessions::Model>,
    /// Raw participant credential, only when newly issued.
    pub participant_token: Option<String>,
    pub rtc: RtcOutcome,
    pub permissions: MediaPermissions,
    pub recording_active: bool,
    pub transcription_active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BulkOutcome {
    pub changed: Vec<Uuid>,
    pub unchanged: Vec<Uuid>,
    pub skipped: Vec<(Uuid, AdmissionState)>,
}

#[derive(Debug, Clone)]
pub struct ModerationOutcome {
    pub participant: conference_participants::Model,
    pub ban_id: Option<Uuid>,
    /// Provider enforcement (media disconnect) of the committed decision.
    pub enforcement: Enforcement,
}

fn participant_payload(p: &conference_participants::Model) -> serde_json::Value {
    json!({
        "conferenceId": p.conference_id,
        "participantId": p.id,
        "role": p.role,
        "admissionState": p.admission_state,
        "identityType": p.identity_type,
        "version": p.version,
    })
}

fn participant_event<'a>(
    space_id: Uuid,
    p: &conference_participants::Model,
    event_type: &'a str,
) -> OutboxRecord<'a> {
    OutboxRecord {
        aggregate_type: "conference_participant",
        aggregate_id: p.id,
        event_type,
        meeting_space_id: Some(space_id),
        conference_id: Some(p.conference_id),
        payload: participant_payload(p),
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Open transaction + locked conference + authorized actor for a management operation.
type ManagementScope = (
    DatabaseTransaction,
    conference_sessions::Model,
    meeting_spaces::Model,
    ConferenceAuthority,
    PermissionContext,
);

pub struct AdmissionService {
    core: Core,
    access: MeetingAccessService,
    invitations: InvitationService,
    rtc: RtcControl,
}

impl AdmissionService {
    pub fn new(core: Core) -> Self {
        Self {
            access: MeetingAccessService::new(core.clone()),
            invitations: InvitationService::new(core.clone()),
            rtc: RtcControl::new(core.clone()),
            core,
        }
    }

    /// After a moderation commit: execute queued provider commands (Part E).
    async fn enforce(&self, conference_id: Uuid, queued: bool) -> Enforcement {
        if !queued {
            return if self.rtc.room_control() {
                Enforcement::NotRequired
            } else {
                Enforcement::Pending
            };
        }
        self.rtc.dispatch_for_conference(conference_id).await
    }

    fn not_live(status: ConferenceStatus) -> ApiError {
        let message = match status {
            ConferenceStatus::Ending => "This conference is ending",
            ConferenceStatus::Ended => "This conference has ended",
            ConferenceStatus::Cancelled => "This conference was cancelled",
            ConferenceStatus::Failed => "This conference is unavailable",
            _ => "This conference has not started yet",
        };
        ApiError::domain(ErrorCode::ConferenceNotLive, message)
    }

    fn ensure_v2(conference: &conference_sessions::Model) -> ApiResult<()> {
        if conference.origin == RecordOrigin::Legacy {
            return Err(state_conflict(
                "This conference is managed by the legacy meeting API until migration certification",
            ));
        }
        Ok(())
    }

    /// Active, unexpired, unrevoked ban matching this subject (space- or conference-scoped).
    pub async fn active_ban<C: ConnectionTrait>(
        conn: &C,
        space_id: Uuid,
        conference_id: Uuid,
        user_id: Option<Uuid>,
        guest_identity: Option<&str>,
        external_ref: Option<&str>,
    ) -> ApiResult<Option<conference_bans::Model>> {
        let mut subject = Condition::any();
        let mut any_subject = false;
        if let Some(u) = user_id {
            subject = subject.add(
                Condition::all()
                    .add(conference_bans::Column::SubjectType.eq(BanSubjectType::User))
                    .add(conference_bans::Column::SubjectRef.eq(u.to_string())),
            );
            any_subject = true;
        }
        if let Some(g) = guest_identity {
            subject = subject.add(
                Condition::all()
                    .add(conference_bans::Column::SubjectType.eq(BanSubjectType::Guest))
                    .add(conference_bans::Column::SubjectRef.eq(g)),
            );
            any_subject = true;
        }
        if let Some(e) = external_ref {
            subject = subject.add(
                Condition::all()
                    .add(conference_bans::Column::SubjectType.eq(BanSubjectType::External))
                    .add(conference_bans::Column::SubjectRef.eq(e.to_ascii_lowercase())),
            );
            any_subject = true;
        }
        if !any_subject {
            return Ok(None);
        }
        let now = Utc::now();
        Ok(conference_bans::Entity::find()
            .filter(conference_bans::Column::MeetingSpaceId.eq(space_id))
            .filter(conference_bans::Column::RevokedAt.is_null())
            .filter(
                Condition::any()
                    .add(conference_bans::Column::ExpiresAt.is_null())
                    .add(conference_bans::Column::ExpiresAt.gt(now)),
            )
            .filter(
                Condition::any()
                    .add(conference_bans::Column::Scope.eq(BanScope::Space))
                    .add(conference_bans::Column::ConferenceId.eq(conference_id)),
            )
            .filter(subject)
            .one(conn)
            .await?)
    }

    async fn is_banned<C: ConnectionTrait>(
        conn: &C,
        space_id: Uuid,
        p: &conference_participants::Model,
    ) -> ApiResult<bool> {
        Ok(Self::active_ban(
            conn,
            space_id,
            p.conference_id,
            p.user_id,
            p.guest_identity.as_deref(),
            p.external_ref.as_deref(),
        )
        .await?
        .is_some())
    }

    /// Seats: interactive roles count against `interactive_capacity`; VIEWERs against
    /// `viewer_capacity` (or the interactive pool when no viewer pool is configured).
    async fn ensure_capacity<C: ConnectionTrait>(
        conn: &C,
        space: &meeting_spaces::Model,
        conference_id: Uuid,
        role: Role,
    ) -> ApiResult<()> {
        let viewer_pool = role == Role::Viewer && space.viewer_capacity > 0;
        let mut q = conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::AdmissionState.eq(AdmissionState::Admitted));
        let limit = if viewer_pool {
            q = q.filter(conference_participants::Column::Role.eq(Role::Viewer));
            space.viewer_capacity
        } else {
            if space.viewer_capacity > 0 {
                q = q.filter(conference_participants::Column::Role.ne(Role::Viewer));
            }
            space.interactive_capacity
        };
        if q.count(conn).await? >= limit as u64 {
            return Err(ApiError::domain(
                ErrorCode::MeetingCapacityReached,
                "This meeting is full",
            ));
        }
        Ok(())
    }

    fn is_internal(space: &meeting_spaces::Model, ctx: &ActorContext) -> bool {
        let Some(r) = ctx.registered() else {
            return false;
        };
        if let Some(org) = space.organization_id {
            if ctx.verified_organizations.contains(&org) {
                return true;
            }
        }
        let domains = access_config(space).allowed_domains;
        r.verified_domain()
            .map(|d| domains.iter().any(|a| a.eq_ignore_ascii_case(&d)))
            .unwrap_or(false)
    }

    async fn custom_waiting(
        &self,
        space: &meeting_spaces::Model,
        ctx: &ActorContext,
        role: Role,
    ) -> Option<bool> {
        if space.waiting_room_policy != WaitingRoomPolicy::Custom {
            return None;
        }
        let name = access_config(space).waiting_room_adapter_ref?;
        let evaluator = self.core.adapters.waiting_room.get(&name)?.clone();
        Some(evaluator.must_wait(space.id, ctx.user_id(), role).await)
    }

    async fn expire_stale_sessions<C: ConnectionTrait>(
        conn: &C,
        participant_id: Uuid,
    ) -> ApiResult<()> {
        let now = Utc::now();
        participant_sessions::Entity::update_many()
            .col_expr(
                participant_sessions::Column::State,
                Expr::value(SessionState::Expired.as_str()),
            )
            .col_expr(
                participant_sessions::Column::DisconnectedAt,
                Expr::value(now),
            )
            .col_expr(
                participant_sessions::Column::EndReason,
                Expr::value("EXPIRED"),
            )
            .col_expr(participant_sessions::Column::UpdatedAt, Expr::value(now))
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant_id))
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            )
            .filter(participant_sessions::Column::ExpiresAt.lte(now))
            .exec(conn)
            .await?;
        Ok(())
    }

    async fn active_sessions<C: ConnectionTrait>(
        conn: &C,
        participant_id: Uuid,
    ) -> ApiResult<Vec<participant_sessions::Model>> {
        Ok(participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant_id))
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            )
            .all(conn)
            .await?)
    }

    /// New device/network session for an ADMITTED participant. Media identity is
    /// server-generated; a reconnect may supersede an earlier session of the SAME
    /// participant only.
    async fn establish_session(
        &self,
        txn: &DatabaseTransaction,
        participant: &conference_participants::Model,
        previous_session_id: Option<Uuid>,
        client_kind: Option<String>,
    ) -> ApiResult<participant_sessions::Model> {
        Self::expire_stale_sessions(txn, participant.id).await?;
        let now = Utc::now();
        let mut reconnect_of = None;
        if let Some(prev) = previous_session_id {
            let previous = participant_sessions::Entity::find_by_id(prev)
                .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant.id))
                .filter(participant_sessions::Column::ConferenceId.eq(participant.conference_id))
                .one(txn)
                .await?
                .ok_or_else(|| {
                    validation("previousSessionId does not belong to this participant")
                })?;
            if previous.state.is_active() {
                let mut am: participant_sessions::ActiveModel = previous.clone().into();
                am.state = Set(SessionState::Replaced);
                am.disconnected_at = Set(Some(now));
                am.end_reason = Set(Some("RECONNECTED".into()));
                am.updated_at = Set(now);
                am.update(txn).await?;
            }
            reconnect_of = Some(previous.id);
        }
        let active = Self::active_sessions(txn, participant.id).await?.len() as u64;
        if active >= self.core.config.max_sessions_per_participant {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Too many active devices for this participant",
            ));
        }
        let id = Uuid::new_v4();
        Ok(participant_sessions::ActiveModel {
            id: Set(id),
            conference_participant_id: Set(participant.id),
            conference_id: Set(participant.conference_id),
            media_identity: Set(format!("ps_{}", id.simple())),
            state: Set(SessionState::Pending),
            established_at: Set(now),
            connected_at: Set(None),
            disconnected_at: Set(None),
            expires_at: Set(Some(
                now + Duration::seconds(self.core.config.session_ttl_secs),
            )),
            end_reason: Set(None),
            reconnect_of_session_id: Set(reconnect_of),
            client_kind: Set(client_kind),
            legacy_participant_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?)
    }

    /// `POST /api/v2/conferences/{id}/join`
    pub async fn join(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: JoinConferenceRequest,
    ) -> ApiResult<JoinOutcome> {
        if req
            .external_context
            .as_ref()
            .map(|c| c.len() > 512)
            .unwrap_or(false)
        {
            return Err(validation("externalContext must be at most 512 characters"));
        }
        // 1. Resolve conference / space (fail fast; re-checked under lock below).
        let conference = find_conference(&self.core.db, conference_id).await?;
        Self::ensure_v2(&conference)?;
        let space = find_space(&self.core.db, conference.meeting_space_id)
            .await
            .map_err(|_| crate::services::meeting_core::not_found_conference())?;
        // 2. Actor is already server-resolved. A participant credential must belong to
        //    THIS conference; credentials of other conferences/guests are rejected.
        let existing = own_participant(&self.core.db, conference_id, ctx, false).await?;
        if matches!(ctx.kind, ActorKind::ParticipantCredential(_)) && existing.is_none() {
            return Err(ApiError::domain(
                ErrorCode::MeetingAccessDenied,
                "This participant credential is not valid for this conference",
            ));
        }
        if matches!(ctx.kind, ActorKind::System(_)) {
            return Err(permission_denied("System actors cannot join conferences"));
        }
        // 3./4. Lifecycle and space status.
        if conference.status != ConferenceStatus::Live {
            return Err(Self::not_live(conference.status));
        }
        if space.status != SpaceStatus::Active {
            return Err(ApiError::domain(
                ErrorCode::MeetingAccessDenied,
                "This meeting is no longer active",
            ));
        }
        // 5./6. Access policy (membership / invitation / passcode / adapters).
        let invitation = match req.invitation_token.as_deref() {
            Some(t) => Some(
                self.invitations
                    .validate_for_join(ctx, t, space.id, conference_id)
                    .await?,
            ),
            None => None,
        };
        let decision = self
            .access
            .evaluate(AccessRequest {
                space: &space,
                conference_id,
                actor: ctx,
                existing_participant: existing.as_ref(),
                invitation,
                passcode: req.passcode.as_deref(),
                external_context: req.external_context.as_deref(),
            })
            .await?;
        let display_name = match (&req.display_name, ctx.registered(), &existing) {
            (Some(n), _, _) => sanitize_display_name(n)?,
            (None, Some(r), _) => r.display_name.chars().take(100).collect(),
            (None, None, Some(p)) => p.display_name.clone(),
            (None, None, None) => {
                return Err(validation("displayName is required to join as a guest"))
            }
        };

        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        if conference.status != ConferenceStatus::Live {
            return Err(Self::not_live(conference.status));
        }
        let space = find_space(&txn, conference.meeting_space_id).await?;
        if space.status != SpaceStatus::Active {
            return Err(ApiError::domain(
                ErrorCode::MeetingAccessDenied,
                "This meeting is no longer active",
            ));
        }
        let existing = own_participant(&txn, conference_id, ctx, true).await?;
        let pctx = PermissionContext::for_space(&space);

        // 10. Role (authoritative sources only). Live-assigned roles survive reconnect.
        let (mut role, mut role_source) = resolve_role(&decision, space.default_role);
        if let Some(p) = &existing {
            if !decision.is_owner {
                match p.role_source {
                    RoleSource::Assigned => {
                        role = p.role;
                        role_source = RoleSource::Assigned;
                    }
                    // A consumed invitation keeps granting its role on reconnect.
                    RoleSource::Invitation
                        if decision.invitation.is_none() && p.role.rank() > role.rank() =>
                    {
                        role = p.role;
                        role_source = RoleSource::Invitation;
                    }
                    _ => {}
                }
            }
        }
        let exempt = pctx.allows(role, Permission::ParticipantAdmit);

        // 7. Lock: no new non-exempt entrants. Admitted participants may reconnect and
        //    already-waiting participants keep waiting.
        let already_in = existing
            .as_ref()
            .map(|p| {
                matches!(
                    p.admission_state,
                    AdmissionState::Admitted | AdmissionState::Waiting
                )
            })
            .unwrap_or(false);
        if conference.is_locked && !exempt && !already_in {
            return Err(ApiError::domain(
                ErrorCode::ConferenceLocked,
                "This conference is locked",
            ));
        }

        // 8. Bans (identity-based, never display-name based).
        let guest_identity = existing.as_ref().and_then(|p| p.guest_identity.clone());
        let external_ref = existing
            .as_ref()
            .and_then(|p| p.external_ref.clone())
            .or_else(|| {
                decision
                    .invitation
                    .as_ref()
                    .and_then(|i| i.invitee_email.clone())
            });
        let banned = existing
            .as_ref()
            .map(|p| p.admission_state == AdmissionState::Banned)
            .unwrap_or(false)
            || Self::active_ban(
                &txn,
                space.id,
                conference_id,
                ctx.user_id(),
                guest_identity.as_deref(),
                external_ref.as_deref(),
            )
            .await?
            .is_some();
        if banned {
            return Err(ApiError::domain(
                ErrorCode::ParticipantBanned,
                "You are banned from this meeting",
            ));
        }
        if existing
            .as_ref()
            .map(|p| p.admission_state == AdmissionState::Removed)
            .unwrap_or(false)
        {
            return Err(ApiError::domain(
                ErrorCode::MeetingAccessDenied,
                "You have been removed from this meeting",
            ));
        }

        // 11. Waiting room.
        let identity_type = match (&existing, ctx.registered(), &decision.invitation) {
            (Some(p), _, _) => p.identity_type,
            (None, Some(_), _) => IdentityType::RegisteredUser,
            (None, None, Some(_)) => IdentityType::InvitedExternal,
            (None, None, None) => IdentityType::Guest,
        };
        let wait = must_wait(&WaitingInput {
            policy: space.waiting_room_policy,
            identity_type,
            exempt,
            is_member: decision.is_owner || decision.membership.is_some(),
            is_internal: decision.is_owner || Self::is_internal(&space, ctx),
            custom: self.custom_waiting(&space, ctx, role).await,
        });
        let previous_state = existing.as_ref().map(|p| p.admission_state);
        let target = match previous_state {
            Some(AdmissionState::Admitted) => AdmissionState::Admitted,
            Some(AdmissionState::Denied) if !exempt => AdmissionState::Waiting,
            _ if wait => AdmissionState::Waiting,
            _ => AdmissionState::Admitted,
        };
        let newly_admitted =
            target == AdmissionState::Admitted && previous_state != Some(AdmissionState::Admitted);
        if newly_admitted && !exempt {
            Self::ensure_capacity(&txn, &space, conference_id, role).await?;
        }

        // 12. Commit the logical participant (one per person per conference).
        let now = Utc::now();
        let mut participant_token = None;
        let consume_invitation = decision
            .invitation
            .as_ref()
            .filter(|i| {
                existing
                    .as_ref()
                    .map(|p| p.invitation_id != Some(i.id))
                    .unwrap_or(true)
            })
            .map(|i| i.id);
        let participant = match existing.clone() {
            Some(p) => {
                let mut am: conference_participants::ActiveModel = p.clone().into();
                let changed =
                    p.role != role || p.admission_state != target || p.display_name != display_name;
                am.role = Set(role);
                am.role_source = Set(role_source);
                am.display_name = Set(display_name.clone());
                am.admission_state = Set(target);
                if newly_admitted {
                    am.admitted_at = Set(Some(now));
                    am.admitted_by = Set(Some("system:admission-policy".into()));
                }
                if previous_state == Some(AdmissionState::Denied)
                    && target == AdmissionState::Waiting
                {
                    am.requested_at = Set(now);
                }
                if let Some(inv) = consume_invitation {
                    am.invitation_id = Set(Some(inv));
                }
                if changed || consume_invitation.is_some() {
                    am.version = Set(p.version + 1);
                    am.updated_at = Set(now);
                    am.update(&txn).await?
                } else {
                    p
                }
            }
            None => {
                let registered = ctx.registered();
                let (guest_identity, credential_hash) = if registered.is_none() {
                    let token = secrets::generate_bearer_secret("pt_");
                    let hash = secrets::hash_secret(&token);
                    participant_token = Some(token);
                    (Some(secrets::generate_opaque_id("gst_")), Some(hash))
                } else {
                    (None, None)
                };
                conference_participants::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    conference_id: Set(conference_id),
                    identity_type: Set(identity_type),
                    user_id: Set(registered.map(|r| r.user_id)),
                    external_ref: Set(external_ref.clone()),
                    guest_identity: Set(guest_identity),
                    credential_hash: Set(credential_hash),
                    invitation_id: Set(consume_invitation),
                    legacy_participant_id: Set(None),
                    display_name: Set(display_name.clone()),
                    role: Set(role),
                    role_source: Set(role_source),
                    admission_state: Set(target),
                    requested_at: Set(now),
                    admitted_at: Set((target == AdmissionState::Admitted).then_some(now)),
                    admitted_by: Set((target == AdmissionState::Admitted)
                        .then(|| "system:admission-policy".into())),
                    denied_at: Set(None),
                    denied_by: Set(None),
                    denial_reason_code: Set(None),
                    denial_message: Set(None),
                    denial_internal_note: Set(None),
                    removed_at: Set(None),
                    removed_by: Set(None),
                    banned_at: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                    version: Set(1),
                }
                .insert(&txn)
                .await?
            }
        };
        if let Some(inv) = consume_invitation {
            invitations::consume(&txn, inv).await?;
            audit(
                &txn,
                &AuditActor::participant(participant.id),
                AuditRecord {
                    meeting_space_id: space.id,
                    conference_id: Some(conference_id),
                    action: action::INVITATION_USE,
                    target_type: Some("INVITATION"),
                    target_ref: Some(inv.to_string()),
                    metadata: json!({ "participantId": participant.id }),
                },
            )
            .await?;
        }
        if previous_state != Some(participant.admission_state) {
            audit(
                &txn,
                &AuditActor::participant(participant.id),
                AuditRecord {
                    meeting_space_id: space.id,
                    conference_id: Some(conference_id),
                    action: if newly_admitted {
                        action::ADMIT
                    } else {
                        action::JOIN_REQUEST
                    },
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(participant.id.to_string()),
                    metadata: json!({
                        "identityType": participant.identity_type,
                        "role": participant.role,
                        "admissionState": participant.admission_state,
                        "grant": format!("{:?}", decision.grant),
                        "automatic": true,
                    }),
                },
            )
            .await?;
            let e = if newly_admitted {
                event::PARTICIPANT_ADMITTED
            } else {
                event::PARTICIPANT_REQUESTED_JOIN
            };
            emit(&txn, participant_event(space.id, &participant, e)).await?;
        }

        // 13. Device session only for an ADMITTED participant.
        let session = if participant.admission_state == AdmissionState::Admitted {
            Some(
                self.establish_session(
                    &txn,
                    &participant,
                    req.previous_session_id,
                    ctx.client_kind.clone(),
                )
                .await?,
            )
        } else {
            None
        };
        txn.commit().await?;

        // 14. RTC credentials only after the authoritative state committed.
        let permissions = if participant.admission_state == AdmissionState::Admitted {
            pctx.media(participant.role)
        } else {
            MediaPermissions::none()
        };
        let rtc = match &session {
            None => RtcOutcome::NotIssuedWaiting,
            Some(_) if !self.core.flags.rtc_provider_control_v2 => {
                RtcOutcome::ProviderControlDisabled
            }
            Some(s) => {
                // Part E: a credential is only minted for a room the provider confirms now.
                if self.rtc.room_control() {
                    if let Err(e) = self.rtc.verify_room_for_join(conference_id).await {
                        inc(&M.credentials_refused);
                        self.close_session(
                            s.id,
                            SessionState::Terminated,
                            "RTC_PROVIDER_UNAVAILABLE",
                        )
                        .await?;
                        return Err(match e.code() {
                            Some(ErrorCode::ConferenceStateConflict) => Self::not_live(
                                find_conference(&self.core.db, conference_id).await?.status,
                            ),
                            _ => e,
                        });
                    }
                }
                let ttl = if self.rtc.room_control() {
                    self.rtc.credential_ttl()
                } else {
                    StdDuration::from_secs(self.core.config.session_ttl_secs as u64)
                };
                let creds = self
                    .core
                    .rtc
                    .issue_join_credentials(&JoinCredentialRequest {
                        participant: RtcParticipantRef::new(&conference, s),
                        display_name: participant.display_name.clone(),
                        permissions,
                        ttl,
                    })
                    .await;
                let creds = match creds {
                    Ok(c) => c,
                    Err(e) => {
                        // Business state stays authoritative; the session is not usable.
                        self.close_session(
                            s.id,
                            SessionState::Terminated,
                            "RTC_PROVIDER_UNAVAILABLE",
                        )
                        .await?;
                        return Err(e.into());
                    }
                };
                // Fail closed if the conference left LIVE (or the session/participant was
                // revoked) while credentials were minted.
                let current = find_conference(&self.core.db, conference_id).await?;
                if current.status != ConferenceStatus::Live {
                    self.close_session(s.id, SessionState::Terminated, "CONFERENCE_CLOSING")
                        .await?;
                    inc(&M.credentials_refused);
                    return Err(Self::not_live(current.status));
                }
                let still_active = participant_sessions::Entity::find_by_id(s.id)
                    .one(&self.core.db)
                    .await?
                    .map(|row| row.state.is_active())
                    .unwrap_or(false);
                if !still_active {
                    inc(&M.credentials_refused);
                    return Err(ApiError::domain(
                        ErrorCode::MeetingAccessDenied,
                        "Your access to this conference changed while joining",
                    ));
                }
                inc(&M.credentials_issued);
                RtcOutcome::Issued(creds)
            }
        };
        let rtc = if participant.admission_state == AdmissionState::Admitted {
            rtc
        } else {
            RtcOutcome::NotIssuedWaiting
        };
        Ok(JoinOutcome {
            recording_active: conference.recording_state != "OFF"
                && space.recording_policy != MediaCapturePolicy::Disabled,
            transcription_active: conference.transcription_state != "OFF",
            conference,
            participant,
            session,
            participant_token,
            rtc,
            permissions,
        })
    }

    async fn close_session(
        &self,
        session_id: Uuid,
        state: SessionState,
        reason: &str,
    ) -> ApiResult<()> {
        let now = Utc::now();
        participant_sessions::Entity::update_many()
            .col_expr(
                participant_sessions::Column::State,
                Expr::value(state.as_str()),
            )
            .col_expr(
                participant_sessions::Column::DisconnectedAt,
                Expr::value(now),
            )
            .col_expr(participant_sessions::Column::EndReason, Expr::value(reason))
            .col_expr(participant_sessions::Column::UpdatedAt, Expr::value(now))
            .filter(participant_sessions::Column::Id.eq(session_id))
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            )
            .exec(&self.core.db)
            .await?;
        Ok(())
    }

    /// `POST /api/v2/conferences/{id}/leave` — closes the caller's own session(s).
    pub async fn leave(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        session_id: Option<Uuid>,
    ) -> ApiResult<u64> {
        let txn = self.core.db.begin().await?;
        // Lock order: conference → participant (the event sequence also locks the row).
        let conference = lock_conference(&txn, conference_id).await?;
        let participant = own_participant(&txn, conference_id, ctx, true)
            .await?
            .ok_or_else(not_found_participant)?;
        let mut q = participant_sessions::Entity::find()
            .filter(participant_sessions::Column::ConferenceParticipantId.eq(participant.id))
            .filter(
                participant_sessions::Column::State
                    .is_in([SessionState::Pending, SessionState::Connected]),
            );
        if let Some(sid) = session_id {
            q = q.filter(participant_sessions::Column::Id.eq(sid));
        }
        let sessions = q.all(&txn).await?;
        let now = Utc::now();
        for s in &sessions {
            let mut am: participant_sessions::ActiveModel = s.clone().into();
            am.state = Set(SessionState::Disconnected);
            am.disconnected_at = Set(Some(now));
            am.end_reason = Set(Some("LEFT".into()));
            am.updated_at = Set(now);
            am.update(&txn).await?;
        }
        if !sessions.is_empty() {
            let mut payload = participant_payload(&participant);
            payload["sessionIds"] = json!(sessions.iter().map(|s| s.id).collect::<Vec<_>>());
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "conference_participant",
                    aggregate_id: participant.id,
                    event_type: event::PARTICIPANT_LEFT,
                    meeting_space_id: Some(conference.meeting_space_id),
                    conference_id: Some(conference_id),
                    payload,
                },
            )
            .await?;
        }
        txn.commit().await?;
        Ok(sessions.len() as u64)
    }

    pub async fn me(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<conference_participants::Model> {
        find_conference(&self.core.db, conference_id).await?;
        own_participant(&self.core.db, conference_id, ctx, false)
            .await?
            .ok_or_else(not_found_participant)
    }

    // -- management ---------------------------------------------------------

    async fn begin_management(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        permission: Permission,
    ) -> ApiResult<ManagementScope> {
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        Self::ensure_v2(&conference)?;
        let space = find_space(&txn, conference.meeting_space_id).await?;
        let authority = conference_authority(&txn, &space, conference_id, ctx)
            .await?
            .ok_or_else(|| permission_denied("You are not permitted to manage this conference"))?;
        let pctx = PermissionContext::for_space(&space);
        if !pctx.allows(authority.role, permission) {
            return Err(permission_denied("Your role does not allow this action"));
        }
        Ok((txn, conference, space, authority, pctx))
    }

    /// Admitted participants (roster) — visible to anyone with conference authority.
    pub async fn roster(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<Vec<conference_participants::Model>> {
        let conference = find_conference(&self.core.db, conference_id).await?;
        let space = find_space(&self.core.db, conference.meeting_space_id).await?;
        conference_authority(&self.core.db, &space, conference_id, ctx)
            .await?
            .ok_or_else(|| permission_denied("You are not part of this conference"))?;
        Ok(conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::AdmissionState.eq(AdmissionState::Admitted))
            .order_by_asc(conference_participants::Column::AdmittedAt)
            .all(&self.core.db)
            .await?)
    }

    pub async fn waiting_room(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
    ) -> ApiResult<Vec<conference_participants::Model>> {
        self.require_waiting_room_feature()?;
        let (txn, _, _, _, _) = self
            .begin_management(ctx, conference_id, Permission::ParticipantAdmit)
            .await?;
        let list = conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::AdmissionState.eq(AdmissionState::Waiting))
            .order_by_asc(conference_participants::Column::RequestedAt)
            .all(&txn)
            .await?;
        txn.commit().await?;
        Ok(list)
    }

    fn require_waiting_room_feature(&self) -> ApiResult<()> {
        if self.core.flags.waiting_room_v2_enabled {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Waiting room v2 is not enabled",
            ))
        }
    }

    /// Lock targets in a deterministic order. Any id outside this conference fails the
    /// whole request before any mutation (cross-conference safety).
    async fn lock_targets(
        txn: &DatabaseTransaction,
        conference_id: Uuid,
        ids: &[Uuid],
        all_waiting: bool,
    ) -> ApiResult<Vec<conference_participants::Model>> {
        if all_waiting {
            return Ok(conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                .filter(conference_participants::Column::AdmissionState.eq(AdmissionState::Waiting))
                .order_by_asc(conference_participants::Column::Id)
                .lock(LockType::Update)
                .all(txn)
                .await?);
        }
        if ids.is_empty() {
            return Err(validation("Provide participantIds or set all=true"));
        }
        if ids.len() > 500 {
            return Err(validation("At most 500 participants per request"));
        }
        let mut unique: Vec<Uuid> = ids.to_vec();
        unique.sort();
        unique.dedup();
        let found = conference_participants::Entity::find()
            .filter(conference_participants::Column::ConferenceId.eq(conference_id))
            .filter(conference_participants::Column::Id.is_in(unique.clone()))
            .order_by_asc(conference_participants::Column::Id)
            .lock(LockType::Update)
            .all(txn)
            .await?;
        if found.len() != unique.len() {
            return Err(not_found_participant());
        }
        Ok(found)
    }

    /// Bulk/single admission: transactional, idempotent, scope-safe.
    pub async fn admit(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        ids: &[Uuid],
        all: bool,
    ) -> ApiResult<BulkOutcome> {
        self.require_waiting_room_feature()?;
        let (txn, conference, space, authority, pctx) = self
            .begin_management(ctx, conference_id, Permission::ParticipantAdmit)
            .await?;
        let targets = Self::lock_targets(&txn, conference_id, ids, all).await?;
        if conference.status != ConferenceStatus::Live {
            return Err(Self::not_live(conference.status));
        }
        let mut out = BulkOutcome::default();
        let now = Utc::now();
        for p in targets {
            match p.admission_state {
                AdmissionState::Admitted => out.unchanged.push(p.id),
                AdmissionState::Waiting => {
                    if Self::is_banned(&txn, space.id, &p).await? {
                        out.skipped.push((p.id, AdmissionState::Banned));
                        continue;
                    }
                    if !pctx.allows(p.role, Permission::ParticipantAdmit) {
                        Self::ensure_capacity(&txn, &space, conference_id, p.role).await?;
                    }
                    let mut am: conference_participants::ActiveModel = p.clone().into();
                    am.admission_state = Set(AdmissionState::Admitted);
                    am.admitted_at = Set(Some(now));
                    am.admitted_by = Set(Some(authority.audit.reference()));
                    am.version = Set(p.version + 1);
                    am.updated_at = Set(now);
                    let updated = am.update(&txn).await?;
                    audit(
                        &txn,
                        &authority.audit,
                        AuditRecord {
                            meeting_space_id: space.id,
                            conference_id: Some(conference_id),
                            action: action::ADMIT,
                            target_type: Some("PARTICIPANT"),
                            target_ref: Some(p.id.to_string()),
                            metadata: json!({ "bulk": all || ids.len() > 1 }),
                        },
                    )
                    .await?;
                    emit(
                        &txn,
                        participant_event(space.id, &updated, event::PARTICIPANT_ADMITTED),
                    )
                    .await?;
                    out.changed.push(p.id);
                }
                other => out.skipped.push((p.id, other)),
            }
        }
        txn.commit().await?;
        Ok(out)
    }

    pub async fn deny(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: &DenyRequest,
    ) -> ApiResult<BulkOutcome> {
        self.require_waiting_room_feature()?;
        if req
            .message
            .as_ref()
            .map(|m| m.chars().count() > 500)
            .unwrap_or(false)
        {
            return Err(validation("message must be at most 500 characters"));
        }
        if req
            .reason_code
            .as_ref()
            .map(|c| c.len() > 64)
            .unwrap_or(false)
        {
            return Err(validation("reasonCode must be at most 64 characters"));
        }
        let (txn, _conference, space, authority, _) = self
            .begin_management(ctx, conference_id, Permission::ParticipantDeny)
            .await?;
        let targets =
            Self::lock_targets(&txn, conference_id, &req.participant_ids, req.all).await?;
        let mut out = BulkOutcome::default();
        let now = Utc::now();
        for p in targets {
            match p.admission_state {
                AdmissionState::Denied => out.unchanged.push(p.id),
                AdmissionState::Waiting => {
                    let mut am: conference_participants::ActiveModel = p.clone().into();
                    am.admission_state = Set(AdmissionState::Denied);
                    am.denied_at = Set(Some(now));
                    am.denied_by = Set(Some(authority.audit.reference()));
                    am.denial_reason_code = Set(req.reason_code.clone());
                    am.denial_message = Set(req.message.clone());
                    am.denial_internal_note = Set(req.internal_note.clone());
                    am.version = Set(p.version + 1);
                    am.updated_at = Set(now);
                    let updated = am.update(&txn).await?;
                    audit(
                        &txn,
                        &authority.audit,
                        AuditRecord {
                            meeting_space_id: space.id,
                            conference_id: Some(conference_id),
                            action: action::DENY,
                            target_type: Some("PARTICIPANT"),
                            target_ref: Some(p.id.to_string()),
                            metadata: json!({ "reasonCode": req.reason_code }),
                        },
                    )
                    .await?;
                    emit(
                        &txn,
                        participant_event(space.id, &updated, event::PARTICIPANT_DENIED),
                    )
                    .await?;
                    out.changed.push(p.id);
                }
                other => out.skipped.push((p.id, other)),
            }
        }
        txn.commit().await?;
        Ok(out)
    }

    /// Queue provider removal for every active session (Part E enforces).
    async fn queue_session_removal(
        txn: &DatabaseTransaction,
        space_id: Uuid,
        conference: &conference_sessions::Model,
        participant_id: Uuid,
        sessions: &[participant_sessions::Model],
        reason: &str,
    ) -> ApiResult<()> {
        for s in sessions {
            emit(
                txn,
                OutboxRecord {
                    aggregate_type: "participant_session",
                    aggregate_id: s.id,
                    event_type: event::RTC_PARTICIPANT_REMOVE_REQUESTED,
                    meeting_space_id: Some(space_id),
                    conference_id: Some(conference.id),
                    payload: json!({
                        "conferenceId": conference.id,
                        "participantId": participant_id,
                        "participantSessionId": s.id,
                        "mediaIdentity": s.media_identity,
                        "reason": reason,
                    }),
                },
            )
            .await?;
        }
        Ok(())
    }

    fn check_target(
        authority: &ConferenceAuthority,
        target: &conference_participants::Model,
    ) -> ApiResult<()> {
        if authority
            .participant
            .as_ref()
            .map(|p| p.id == target.id)
            .unwrap_or(false)
        {
            return Err(permission_denied("You cannot moderate yourself"));
        }
        if !can_moderate(authority.role, target.role) {
            return Err(permission_denied(
                "You can only moderate less privileged participants",
            ));
        }
        Ok(())
    }

    pub async fn return_to_waiting(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
    ) -> ApiResult<ModerationOutcome> {
        self.require_waiting_room_feature()?;
        let (txn, conference, space, authority, _) = self
            .begin_management(ctx, conference_id, Permission::ParticipantReturnToWaiting)
            .await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        Self::check_target(&authority, &target)?;
        if target.admission_state == AdmissionState::Waiting {
            txn.commit().await?;
            return Ok(ModerationOutcome {
                participant: target,
                ban_id: None,
                enforcement: Enforcement::NotRequired,
            });
        }
        if target.admission_state != AdmissionState::Admitted {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Only an admitted participant can be returned to waiting",
            ));
        }
        let sessions = Self::active_sessions(&txn, target.id).await?;
        let now = Utc::now();
        let mut am: conference_participants::ActiveModel = target.clone().into();
        am.admission_state = Set(AdmissionState::Waiting);
        am.requested_at = Set(now);
        am.version = Set(target.version + 1);
        am.updated_at = Set(now);
        // DB trigger terminates every active session in this same transaction.
        let updated = am.update(&txn).await?;
        Self::queue_session_removal(
            &txn,
            space.id,
            &conference,
            target.id,
            &sessions,
            "RETURNED_TO_WAITING",
        )
        .await?;
        audit(
            &txn,
            &authority.audit,
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: Some(conference_id),
                action: action::RETURN_TO_WAITING,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "terminatedSessions": sessions.len() }),
            },
        )
        .await?;
        emit(
            &txn,
            participant_event(space.id, &updated, event::PARTICIPANT_RETURNED_TO_WAITING),
        )
        .await?;
        txn.commit().await?;
        let enforcement = self.enforce(conference_id, !sessions.is_empty()).await;
        Ok(ModerationOutcome {
            participant: updated,
            ban_id: None,
            enforcement,
        })
    }

    pub async fn change_role(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        req: &RoleChangeRequest,
    ) -> ApiResult<conference_participants::Model> {
        let (txn, conference, space, authority, pctx) = self
            .begin_management(ctx, conference_id, Permission::ParticipantRoleChange)
            .await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        let is_self = authority
            .participant
            .as_ref()
            .map(|p| p.id == target.id)
            .unwrap_or(false)
            || (ctx.user_id().is_some() && ctx.user_id() == target.user_id);
        check_role_change(authority.role, target.role, req.role, is_self, &pctx)
            .map_err(|d| permission_denied(d.message()))?;
        if let Some(v) = req.version {
            if v != target.version {
                return Err(ApiError::domain(
                    ErrorCode::ParticipantStateConflict,
                    "Participant was modified",
                ));
            }
        }
        if !matches!(
            target.admission_state,
            AdmissionState::Admitted | AdmissionState::Waiting
        ) {
            return Err(ApiError::domain(
                ErrorCode::ParticipantStateConflict,
                "Role can only change for admitted or waiting participants",
            ));
        }
        if target.role == req.role {
            txn.commit().await?;
            return Ok(target);
        }
        let previous = target.role;
        let now = Utc::now();
        let mut am: conference_participants::ActiveModel = target.clone().into();
        am.role = Set(req.role);
        am.role_source = Set(RoleSource::Assigned);
        am.version = Set(target.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        audit(
            &txn,
            &authority.audit,
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: Some(conference_id),
                action: action::ROLE_CHANGE,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "from": previous, "to": req.role }),
            },
        )
        .await?;
        emit(
            &txn,
            participant_event(space.id, &updated, event::PARTICIPANT_ROLE_CHANGED),
        )
        .await?;
        let (before, after) = (pctx.media(previous), pctx.media(req.role));
        if before != after {
            let mut payload = participant_payload(&updated);
            payload["mediaPermissions"] = json!(after);
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "conference_participant",
                    aggregate_id: target.id,
                    event_type: event::PARTICIPANT_MEDIA_PERMISSION_CHANGED,
                    meeting_space_id: Some(space.id),
                    conference_id: Some(conference.id),
                    payload,
                },
            )
            .await?;
        }
        let sessions = Self::active_sessions(&txn, target.id).await?;
        if !sessions.is_empty() {
            emit(
                &txn,
                OutboxRecord {
                    aggregate_type: "conference_participant",
                    aggregate_id: target.id,
                    event_type: event::RTC_PERMISSIONS_UPDATE_REQUESTED,
                    meeting_space_id: Some(space.id),
                    conference_id: Some(conference.id),
                    payload: json!({
                        "conferenceId": conference.id,
                        "participantId": target.id,
                        "mediaIdentities": sessions.iter().map(|s| s.media_identity.clone()).collect::<Vec<_>>(),
                        "permissions": pctx.media(req.role),
                    }),
                },
            )
            .await?;
        }
        txn.commit().await?;
        // Provider grants follow the committed role (executor re-reads the role).
        self.enforce(conference_id, !sessions.is_empty()).await;
        Ok(updated)
    }

    pub async fn remove(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        reason: Option<String>,
    ) -> ApiResult<ModerationOutcome> {
        let (txn, conference, space, authority, _) = self
            .begin_management(ctx, conference_id, Permission::ParticipantRemove)
            .await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        Self::check_target(&authority, &target)?;
        match target.admission_state {
            AdmissionState::Removed | AdmissionState::Banned => {
                txn.commit().await?;
                return Ok(ModerationOutcome {
                    participant: target,
                    ban_id: None,
                    enforcement: Enforcement::NotRequired,
                });
            }
            _ => {}
        }
        let sessions = Self::active_sessions(&txn, target.id).await?;
        let now = Utc::now();
        let mut am: conference_participants::ActiveModel = target.clone().into();
        am.admission_state = Set(AdmissionState::Removed);
        am.removed_at = Set(Some(now));
        am.removed_by = Set(Some(authority.audit.reference()));
        am.version = Set(target.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        Self::queue_session_removal(&txn, space.id, &conference, target.id, &sessions, "REMOVED")
            .await?;
        audit(
            &txn,
            &authority.audit,
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: Some(conference_id),
                action: action::REMOVE,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "reason": reason.map(|r| r.chars().take(500).collect::<String>()) }),
            },
        )
        .await?;
        emit(
            &txn,
            participant_event(space.id, &updated, event::PARTICIPANT_REMOVED),
        )
        .await?;
        txn.commit().await?;
        let enforcement = self.enforce(conference_id, !sessions.is_empty()).await;
        Ok(ModerationOutcome {
            participant: updated,
            ban_id: None,
            enforcement,
        })
    }

    pub async fn ban(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        participant_id: Uuid,
        req: &BanParticipantRequest,
    ) -> ApiResult<ModerationOutcome> {
        if let Some(exp) = req.expires_at {
            if exp <= Utc::now() {
                return Err(validation("expiresAt must be in the future"));
            }
        }
        let (txn, conference, space, authority, _) = self
            .begin_management(ctx, conference_id, Permission::ParticipantBan)
            .await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        Self::check_target(&authority, &target)?;
        if target.admission_state == AdmissionState::Banned {
            txn.commit().await?;
            return Ok(ModerationOutcome {
                participant: target,
                ban_id: None,
                enforcement: Enforcement::NotRequired,
            });
        }
        let (subject_type, subject_ref) =
            match (target.user_id, &target.external_ref, &target.guest_identity) {
                (Some(u), _, _) => (BanSubjectType::User, u.to_string()),
                (None, Some(e), _) => (BanSubjectType::External, e.to_ascii_lowercase()),
                (None, None, Some(g)) => (BanSubjectType::Guest, g.clone()),
                _ => return Err(state_conflict("This participant has no bannable identity")),
            };
        let scope = req.scope.unwrap_or(BanScope::Conference);
        let sessions = Self::active_sessions(&txn, target.id).await?;
        let now = Utc::now();
        let ban = conference_bans::ActiveModel {
            id: Set(Uuid::new_v4()),
            meeting_space_id: Set(space.id),
            conference_id: Set((scope == BanScope::Conference).then_some(conference_id)),
            scope: Set(scope),
            subject_type: Set(subject_type),
            subject_ref: Set(subject_ref),
            participant_id: Set(Some(target.id)),
            reason_code: Set(req.reason_code.clone()),
            reason: Set(req.reason.clone()),
            created_by: Set(authority.audit.reference()),
            created_at: Set(now),
            expires_at: Set(req.expires_at),
            revoked_at: Set(None),
            revoked_by: Set(None),
        }
        .insert(&txn)
        .await?;
        let mut am: conference_participants::ActiveModel = target.clone().into();
        am.admission_state = Set(AdmissionState::Banned);
        am.banned_at = Set(Some(now));
        am.removed_by = Set(Some(authority.audit.reference()));
        am.version = Set(target.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        Self::queue_session_removal(&txn, space.id, &conference, target.id, &sessions, "BANNED")
            .await?;
        audit(
            &txn,
            &authority.audit,
            AuditRecord {
                meeting_space_id: space.id,
                conference_id: Some(conference_id),
                action: action::BAN,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "banId": ban.id, "scope": scope, "reasonCode": req.reason_code, "expiresAt": req.expires_at }),
            },
        )
        .await?;
        emit(
            &txn,
            participant_event(space.id, &updated, event::PARTICIPANT_BANNED),
        )
        .await?;
        txn.commit().await?;
        let enforcement = self.enforce(conference_id, !sessions.is_empty()).await;
        Ok(ModerationOutcome {
            participant: updated,
            ban_id: Some(ban.id),
            enforcement,
        })
    }
}

/// Attendance is derived from session intervals (never from participant timestamps).
/// Returns total connected/established seconds per participant within the conference.
pub async fn attendance_seconds<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    now: DateTime<Utc>,
) -> ApiResult<Vec<(Uuid, i64)>> {
    let sessions = participant_sessions::Entity::find()
        .filter(participant_sessions::Column::ConferenceId.eq(conference_id))
        .order_by_asc(participant_sessions::Column::ConferenceParticipantId)
        .order_by_asc(participant_sessions::Column::EstablishedAt)
        .all(conn)
        .await?;
    Ok(aggregate_attendance(&sessions, now))
}

/// Merge overlapping intervals per participant (multi-device must not double count).
pub fn aggregate_attendance(
    sessions: &[participant_sessions::Model],
    now: DateTime<Utc>,
) -> Vec<(Uuid, i64)> {
    use std::collections::BTreeMap;
    type Interval = (DateTime<Utc>, DateTime<Utc>);
    let mut by_participant: BTreeMap<Uuid, Vec<Interval>> = BTreeMap::new();
    for s in sessions {
        let start = s.connected_at.unwrap_or(s.established_at);
        let end = s
            .disconnected_at
            .unwrap_or(if s.state.is_active() { now } else { start });
        if end > start {
            by_participant
                .entry(s.conference_participant_id)
                .or_default()
                .push((start, end));
        }
    }
    by_participant
        .into_iter()
        .map(|(pid, mut intervals)| {
            intervals.sort();
            let mut total = 0i64;
            let mut current: Option<Interval> = None;
            for (s, e) in intervals {
                current = match current {
                    Some((cs, ce)) if s <= ce => Some((cs, ce.max(e))),
                    Some((cs, ce)) => {
                        total += (ce - cs).num_seconds();
                        Some((s, e))
                    }
                    None => Some((s, e)),
                };
            }
            if let Some((cs, ce)) = current {
                total += (ce - cs).num_seconds();
            }
            (pid, total)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::InvitationStatus;
    use crate::models::{meeting_invitations, meeting_members};

    fn input(policy: WaitingRoomPolicy, identity: IdentityType) -> WaitingInput {
        WaitingInput {
            policy,
            identity_type: identity,
            exempt: false,
            is_member: false,
            is_internal: false,
            custom: None,
        }
    }

    #[test]
    fn waiting_room_policies() {
        use IdentityType::*;
        use WaitingRoomPolicy::*;
        assert!(!must_wait(&input(Off, Guest)));
        assert!(must_wait(&input(Everyone, RegisteredUser)));
        assert!(must_wait(&input(GuestsOnly, Guest)));
        assert!(!must_wait(&input(GuestsOnly, RegisteredUser)));
        assert!(!must_wait(&input(GuestsOnly, InvitedExternal)));
        assert!(must_wait(&input(UnauthenticatedOnly, InvitedExternal)));
        assert!(!must_wait(&input(UnauthenticatedOnly, RegisteredUser)));
        assert!(must_wait(&input(NonMembersOnly, RegisteredUser)));
        assert!(!must_wait(&WaitingInput {
            is_member: true,
            ..input(NonMembersOnly, RegisteredUser)
        }));
        assert!(must_wait(&input(ExternalOnly, RegisteredUser)));
        assert!(!must_wait(&WaitingInput {
            is_internal: true,
            ..input(ExternalOnly, RegisteredUser)
        }));
        assert!(
            must_wait(&input(Custom, Guest)),
            "CUSTOM without decision fails safe"
        );
        assert!(!must_wait(&WaitingInput {
            custom: Some(false),
            ..input(Custom, Guest)
        }));
        assert!(
            !must_wait(&WaitingInput {
                exempt: true,
                ..input(Everyone, Guest)
            }),
            "hosts never wait"
        );
    }

    fn decision() -> AccessDecision {
        AccessDecision {
            grant: crate::services::meeting_access::AccessGrant::Public,
            is_owner: false,
            membership: None,
            invitation: None,
            adapter_role: None,
        }
    }

    #[test]
    fn role_comes_only_from_authoritative_sources() {
        assert_eq!(
            resolve_role(&decision(), Role::Participant),
            (Role::Participant, RoleSource::Default)
        );
        assert_eq!(resolve_role(&decision(), Role::Viewer).0, Role::Viewer);
        let owner = AccessDecision {
            is_owner: true,
            ..decision()
        };
        assert_eq!(
            resolve_role(&owner, Role::Participant),
            (Role::Owner, RoleSource::Owner)
        );
        let now = Utc::now();
        let member = meeting_members::Model {
            id: Uuid::new_v4(),
            meeting_space_id: Uuid::new_v4(),
            principal_type: crate::domain::PrincipalType::User,
            principal_ref: "x".into(),
            user_id: None,
            role: Role::CoHost,
            status: crate::domain::MemberStatus::Active,
            created_by: None,
            created_at: now,
            updated_at: now,
            revoked_at: None,
            revoked_by: None,
            version: 1,
        };
        let inv = meeting_invitations::Model {
            id: Uuid::new_v4(),
            meeting_space_id: Uuid::new_v4(),
            conference_id: None,
            inviter_user_id: Uuid::new_v4(),
            invitee_email: None,
            requested_role: Role::Presenter,
            token_hash: "h".into(),
            expires_at: now,
            use_limit: None,
            usage_count: 0,
            status: InvitationStatus::Active,
            revoked_at: None,
            revoked_by: None,
            created_at: now,
            updated_at: now,
        };
        let both = AccessDecision {
            membership: Some(member),
            invitation: Some(inv),
            ..decision()
        };
        assert_eq!(
            resolve_role(&both, Role::Participant),
            (Role::CoHost, RoleSource::Membership)
        );
    }

    #[test]
    fn display_name_sanitization() {
        assert_eq!(sanitize_display_name("  Ada \u{0007}").unwrap(), "Ada");
        assert!(sanitize_display_name("   ").is_err());
        assert!(sanitize_display_name(&"x".repeat(101)).is_err());
    }

    fn session(
        pid: Uuid,
        start: i64,
        end: Option<i64>,
        base: DateTime<Utc>,
    ) -> participant_sessions::Model {
        participant_sessions::Model {
            id: Uuid::new_v4(),
            conference_participant_id: pid,
            conference_id: Uuid::nil(),
            media_identity: "m".into(),
            state: if end.is_some() {
                SessionState::Disconnected
            } else {
                SessionState::Pending
            },
            established_at: base + Duration::seconds(start),
            connected_at: None,
            disconnected_at: end.map(|e| base + Duration::seconds(e)),
            expires_at: None,
            end_reason: None,
            reconnect_of_session_id: None,
            client_kind: None,
            legacy_participant_id: None,
            created_at: base,
            updated_at: base,
        }
    }

    #[test]
    fn attendance_merges_multi_device_and_reconnect_intervals() {
        let base = Utc::now() - Duration::hours(1);
        let p = Uuid::new_v4();
        let sessions = vec![
            session(p, 0, Some(600), base),     // laptop 0–10m
            session(p, 300, Some(900), base),   // phone 5–15m (overlap)
            session(p, 1200, Some(1500), base), // reconnect 20–25m
        ];
        let out = aggregate_attendance(&sessions, base + Duration::hours(1));
        assert_eq!(out, vec![(p, 900 + 300)]);
    }
}
