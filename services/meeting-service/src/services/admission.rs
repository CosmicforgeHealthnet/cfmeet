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
    sea_query::{Expr, LockType, Query},
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseTransaction, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::{
        AdmissionState, BanScope, BanSubjectType, ConferenceStatus, IdentityType,
        MediaCapturePolicy, MeetingType, RecordOrigin, Role, RoleSource, SessionState, SpaceStatus,
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
    /// Part F: the media context the credential was issued for.
    pub media_context: crate::services::collaboration::breakouts::MediaContext,
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
    ///
    /// The interactive pool is CURRENT occupancy, not lifetime admissions: a logical
    /// participant occupies one seat while it is ADMITTED and has at least one live
    /// ParticipantSession (PENDING / CONNECTED, not past `expires_at`). Several devices
    /// count once; the seat is released when the last live session ends (leave, expiry,
    /// replacement, removal, ban, conference end). ADMITTED itself stays the persistent
    /// admission authorization that lets a participant rejoin without waiting again.
    /// The dedicated viewer pool keeps its admitted-audience count.
    ///
    /// Every seat-taking path runs under the conference row lock (`lock_conference`),
    /// so two requests can never both take the last seat.
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
            let live = Query::select()
                .column(participant_sessions::Column::ConferenceParticipantId)
                .from(participant_sessions::Entity)
                .and_where(participant_sessions::Column::ConferenceId.eq(conference_id))
                .and_where(
                    participant_sessions::Column::State
                        .is_in([SessionState::Pending, SessionState::Connected]),
                )
                .and_where(
                    participant_sessions::Column::ExpiresAt
                        .is_null()
                        .or(participant_sessions::Column::ExpiresAt.gt(Utc::now())),
                )
                .to_owned();
            q = q.filter(conference_participants::Column::Id.in_subquery(live));
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

    /// Seat check for an already-ADMITTED participant that is (re)connecting: one that
    /// still holds a seat (another live device, or the session a reconnect replaces)
    /// needs no new seat; one whose sessions have all ended takes a free seat again.
    /// The dedicated viewer pool counts admitted viewers, so a rejoin needs no new seat.
    async fn ensure_seat<C: ConnectionTrait>(
        conn: &C,
        space: &meeting_spaces::Model,
        participant: &conference_participants::Model,
    ) -> ApiResult<()> {
        if participant.role == Role::Viewer && space.viewer_capacity > 0 {
            return Ok(());
        }
        let now = Utc::now();
        let holds_seat = Self::active_sessions(conn, participant.id)
            .await?
            .iter()
            .any(|s| s.is_live_at(now));
        if holds_seat {
            return Ok(());
        }
        Self::ensure_capacity(conn, space, participant.conference_id, participant.role).await
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
        expires_cap: Option<DateTime<Utc>>,
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
            // Part H: regulated profiles bound guest / externally-authorized sessions
            // (shorter TTL, never beyond the external authorization window).
            expires_at: Set(Some({
                let default = now + Duration::seconds(self.core.config.session_ttl_secs);
                expires_cap.map(|c| c.min(default)).unwrap_or(default)
            })),
            end_reason: Set(None),
            reconnect_of_session_id: Set(reconnect_of),
            client_kind: Set(client_kind),
            legacy_participant_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            screen_share_started_at: Set(None),
            screen_share_track_ref: Set(None),
        }
        .insert(txn)
        .await?)
    }

    /// `POST /api/v2/conferences/{id}/join` — the join pipeline plus the Part I join
    /// SLI (attempt, credential issued, denial vs infrastructure failure; latency to
    /// the media connection is completed by the provider webhook).
    pub async fn join(
        &self,
        ctx: &ActorContext,
        conference_id: Uuid,
        req: JoinConferenceRequest,
    ) -> ApiResult<JoinOutcome> {
        use crate::services::quality::{service as qs, sink::JoinOutcome as Sli};
        let started = Utc::now();
        let result = self.join_inner(ctx, conference_id, req).await;
        let known = conference_sessions::Entity::find_by_id(conference_id)
            .one(&self.core.db)
            .await
            .ok()
            .flatten()
            .is_some();
        if known {
            let (outcome, latency) = match &result {
                Ok(o) => match (&o.rtc, &o.session) {
                    (RtcOutcome::Issued(_), Some(s)) => {
                        let _ = self
                            .core
                            .db
                            .execute(sea_orm::Statement::from_sql_and_values(
                                sea_orm::DbBackend::Postgres,
                                "UPDATE participant_sessions SET join_requested_at = $2 WHERE id = $1",
                                [s.id.into(), started.into()],
                            ))
                            .await;
                        (
                            Sli::Success,
                            Some((Utc::now() - started).num_milliseconds() as f64),
                        )
                    }
                    (RtcOutcome::NotIssuedWaiting, _) => (Sli::Waiting, None),
                    _ => (Sli::Success, None),
                },
                Err(e) => (qs::classify_join_error(e.code()), None),
            };
            qs::record_join(&self.core.db, &self.core, conference_id, outcome, latency).await;
        }
        result
    }

    async fn join_inner(
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
        // Part H: the effective profile policy (meeting type → security profile →
        // organization → space → conference) gates regulated entry.
        let profile = crate::services::profiles::MeetingProfileResolver::load(
            &self.core.db,
            &space,
            Some(&conference),
        )
        .await?;
        let mut decision = decision;
        if !profile.policy.access.public_access_allowed {
            use crate::services::meeting_access::AccessGrant;
            let uncontrolled =
                matches!(decision.grant, AccessGrant::Public | AccessGrant::Passcode)
                    || (decision.grant == AccessGrant::ExistingParticipant
                        && ctx.registered().is_none()
                        && existing
                            .as_ref()
                            .map(|p| p.invitation_id.is_none())
                            .unwrap_or(true)
                        && matches!(
                            space.access_policy,
                            crate::domain::AccessPolicy::Public
                                | crate::domain::AccessPolicy::PublicPasscode
                        ));
            if uncontrolled {
                crate::services::ops_metrics::inc(
                    &crate::services::ops_metrics::OPS_METRICS.profile_policy_denials,
                );
                return Err(ApiError::domain(
                    ErrorCode::ProfilePolicyDenied,
                    "This meeting does not allow uncontrolled public entry",
                ));
            }
        }
        // Part H §49: an external (e.g. clinical) role maps into the generic role model
        // through policy — never OWNER, never from the client.
        if let Some(external) = decision.external.as_ref() {
            if let Some(mapped) = external
                .external_role
                .as_deref()
                .and_then(|r| profile.map_external_role(r))
            {
                decision.adapter_role = Some(mapped);
            }
        }
        // Part H: guest / externally-authorized sessions are bounded by policy and by
        // the external authorization window.
        let session_cap = {
            let mut cap = None;
            if ctx.registered().is_none() || decision.external.is_some() {
                cap = Some(
                    Utc::now()
                        + Duration::seconds(profile.policy.access.guest_session_ttl_secs as i64),
                );
            }
            if let Some(until) = decision.external.as_ref().and_then(|x| x.valid_until) {
                cap = Some(cap.map(|c: DateTime<Utc>| c.min(until)).unwrap_or(until));
            }
            cap
        };
        let collab = self.core.flags.collaboration_v2_enabled;
        // Part F §18: a display name is presentation only; registered users in sensitive
        // meetings always present under their profile name.
        let profile_locked = collab
            && !crate::services::collaboration::prejoin::display_name_editable(
                ctx,
                space.security_profile,
            );
        let display_name = match (&req.display_name, ctx.registered(), &existing) {
            (_, Some(r), _) if profile_locked => r.display_name.chars().take(100).collect(),
            (Some(n), _, _) => sanitize_display_name(n)?,
            (None, Some(r), _) => r.display_name.chars().take(100).collect(),
            (None, None, Some(p)) => p.display_name.clone(),
            (None, None, None) => {
                return Err(validation("displayName is required to join as a guest"))
            }
        };

        // Part F §19 + Part G §21: explicit consents required by policy — and, while a
        // capture runs under EXPLICIT_CONSENT_REQUIRED, that capture's consent — must
        // exist before admission.
        let consent_policy = {
            let c = find_conference(&self.core.db, conference_id).await?;
            let collab_policy = crate::services::collaboration::effective_policy(&space, &c);
            let mut required = if collab {
                collab_policy.required_consents.clone()
            } else {
                vec![]
            };
            if self.core.flags.recording_v2_enabled || self.core.flags.transcription_v2_enabled {
                use crate::services::collaboration::policy::ConsentKind;
                for (active, kind) in [
                    (c.recording_state != "OFF", ConsentKind::Recording),
                    (c.transcription_state != "OFF", ConsentKind::Transcription),
                ] {
                    if active
                        && crate::services::artifacts::consent::mode_for(&profile, kind)
                            == crate::services::profiles::ConsentMode::ExplicitConsentRequired
                        && !required.contains(&kind)
                    {
                        required.push(kind);
                    }
                }
            }
            (!required.is_empty()).then_some((required, collab_policy.consent_version))
        };
        let mut consents_to_record = vec![];
        if let Some((required, version)) = consent_policy.as_ref() {
            let already = match &existing {
                Some(p) => {
                    crate::services::collaboration::prejoin::granted_consents(
                        &self.core.db,
                        p.id,
                        version,
                    )
                    .await?
                }
                None => vec![],
            };
            let request = req.consents.clone().unwrap_or_default();
            let missing = crate::services::collaboration::prejoin::missing_consents(
                required, version, &already, &request,
            );
            if !missing.is_empty() {
                return Err(
                    crate::services::collaboration::prejoin::consent_required_error(&missing),
                );
            }
            consents_to_record = required
                .iter()
                .copied()
                .filter(|k| !already.contains(k))
                .collect();
        }

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
        // Part F §53: webinar attendees (default role) are subscribe-only.
        if collab
            && role_source == RoleSource::Default
            && matches!(
                space.meeting_type,
                MeetingType::Webinar | MeetingType::TownHall
            )
        {
            role = Role::Viewer;
        }
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
            // Part H: regulated profiles force the waiting room (hosts stay exempt).
            policy: if profile.policy.access.waiting_room_forced
                && space.waiting_room_policy == WaitingRoomPolicy::Off
            {
                WaitingRoomPolicy::Everyone
            } else {
                space.waiting_room_policy
            },
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
                let external_changed = decision
                    .external
                    .as_ref()
                    .map(|x| {
                        p.external_role != x.external_role
                            || p.external_authz_ref != x.reference
                            || p.external_authz_valid_until != x.valid_until
                    })
                    .unwrap_or(false);
                let changed = p.role != role
                    || p.admission_state != target
                    || p.display_name != display_name
                    || external_changed;
                am.role = Set(role);
                am.role_source = Set(role_source);
                am.display_name = Set(display_name.clone());
                am.admission_state = Set(target);
                if let Some(x) = decision.external.as_ref() {
                    am.external_role = Set(x.external_role.clone());
                    am.external_authz_ref = Set(x.reference.clone());
                    am.external_authz_valid_until = Set(x.valid_until);
                }
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
                    audio_locked_at: Set(None),
                    audio_locked_by: Set(None),
                    video_locked_at: Set(None),
                    video_locked_by: Set(None),
                    screen_share_locked_at: Set(None),
                    screen_share_locked_by: Set(None),
                    chat_restricted_at: Set(None),
                    chat_restricted_by: Set(None),
                    hand_raised_at: Set(None),
                    hand_lowered_at: Set(None),
                    spotlighted_at: Set(None),
                    spotlighted_by: Set(None),
                    breakout_visit_room_id: Set(None),
                    collab_version: Set(1),
                    external_role: Set(decision
                        .external
                        .as_ref()
                        .and_then(|x| x.external_role.clone())),
                    external_authz_ref: Set(decision
                        .external
                        .as_ref()
                        .and_then(|x| x.reference.clone())),
                    external_authz_valid_until: Set(decision
                        .external
                        .as_ref()
                        .and_then(|x| x.valid_until)),
                    backstage_at: Set(None),
                    registration_id: Set(None),
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

        // Part H §55: regulated profiles keep an enhanced semantic audit of access /
        // join (identifiers and references only — never clinical data).
        if profile.policy.audit.enhanced {
            audit(
                &txn,
                &AuditActor::participant(participant.id),
                AuditRecord {
                    meeting_space_id: space.id,
                    conference_id: Some(conference_id),
                    action: action::PARTICIPANT_JOIN,
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(participant.id.to_string()),
                    metadata: json!({
                        "identityType": participant.identity_type,
                        "role": participant.role,
                        "admissionState": participant.admission_state,
                        "grant": format!("{:?}", decision.grant),
                        "profile": profile.profile,
                    }),
                },
            )
            .await?;
            if let Some(x) = &decision.external {
                audit(
                    &txn,
                    &AuditActor::participant(participant.id),
                    AuditRecord {
                        meeting_space_id: space.id,
                        conference_id: Some(conference_id),
                        action: action::EXTERNAL_AUTHORIZATION,
                        target_type: Some("PARTICIPANT"),
                        target_ref: Some(participant.id.to_string()),
                        metadata: json!({
                            "externalRole": x.external_role,
                            "authorizationRef": x.reference,
                            "validUntil": x.valid_until,
                            "mappedRole": participant.role,
                        }),
                    },
                )
                .await?;
            }
            crate::services::ops_metrics::inc(
                &crate::services::ops_metrics::OPS_METRICS.enhanced_audit_records,
            );
        }

        if let Some((_, version)) = &consent_policy {
            if !consents_to_record.is_empty() {
                crate::services::collaboration::prejoin::record_consents(
                    &txn,
                    space.id,
                    conference_id,
                    participant.id,
                    &consents_to_record,
                    version,
                )
                .await?;
            }
        }

        // 13. Device session only for an ADMITTED participant. Admission persists after
        // leaving; a rejoin without any live session takes a seat again (newly admitted
        // participants were checked above).
        let session = if participant.admission_state == AdmissionState::Admitted {
            if !newly_admitted && !exempt {
                Self::ensure_seat(&txn, &space, &participant).await?;
            }
            Some(
                self.establish_session(
                    &txn,
                    &participant,
                    req.previous_session_id,
                    ctx.client_kind.clone(),
                    session_cap,
                )
                .await?,
            )
        } else {
            None
        };
        txn.commit().await?;

        // 14. RTC credentials only after the authoritative state committed. Part F: the
        // effective projection (role ∧ policy ∧ locks) for the current media context.
        let permissions = if participant.admission_state == AdmissionState::Admitted {
            crate::services::collaboration::media::effective_media_for(
                &self.core.db,
                &conference,
                &participant,
            )
            .await?
        } else {
            MediaPermissions::none()
        };
        let media_context = crate::services::collaboration::breakouts::media_context(
            &self.core.db,
            &conference,
            &participant,
        )
        .await?;
        let rtc = match &session {
            None => RtcOutcome::NotIssuedWaiting,
            Some(_) if !self.core.flags.rtc_provider_control_v2 => {
                RtcOutcome::ProviderControlDisabled
            }
            Some(s) => {
                // Part E: a credential is only minted for a room the provider confirms now.
                if self.rtc.room_control() {
                    if let Err(e) = crate::services::collaboration::breakouts::ensure_context_room(
                        &self.core,
                        &self.rtc,
                        &conference,
                        &media_context,
                    )
                    .await
                    {
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
                        participant: RtcParticipantRef {
                            room_name: media_context.room_name(&conference),
                            ..RtcParticipantRef::new(&conference, s)
                        },
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
            media_context,
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

    /// Part G §22 (`REQUIRE_PARTICIPANT_EXIT`): policy-driven return to the waiting
    /// room by the system (no host authority involved; the decision was the
    /// participant's own consent withdrawal).
    pub async fn system_return_to_waiting(
        &self,
        conference_id: Uuid,
        participant_id: Uuid,
        reason: &str,
    ) -> ApiResult<()> {
        let txn = self.core.db.begin().await?;
        let conference = lock_conference(&txn, conference_id).await?;
        let target = find_scoped_participant(&txn, conference_id, participant_id, true).await?;
        if target.admission_state != AdmissionState::Admitted {
            txn.commit().await?;
            return Ok(());
        }
        let sessions = Self::active_sessions(&txn, target.id).await?;
        let now = Utc::now();
        let mut am: conference_participants::ActiveModel = target.clone().into();
        am.admission_state = Set(AdmissionState::Waiting);
        am.requested_at = Set(now);
        am.version = Set(target.version + 1);
        am.updated_at = Set(now);
        let updated = am.update(&txn).await?;
        Self::queue_session_removal(
            &txn,
            conference.meeting_space_id,
            &conference,
            target.id,
            &sessions,
            reason,
        )
        .await?;
        audit(
            &txn,
            &AuditActor::system("consent-policy"),
            AuditRecord {
                meeting_space_id: conference.meeting_space_id,
                conference_id: Some(conference_id),
                action: action::RETURN_TO_WAITING,
                target_type: Some("PARTICIPANT"),
                target_ref: Some(target.id.to_string()),
                metadata: json!({ "reason": reason, "terminatedSessions": sessions.len() }),
            },
        )
        .await?;
        emit(
            &txn,
            participant_event(
                conference.meeting_space_id,
                &updated,
                event::PARTICIPANT_RETURNED_TO_WAITING,
            ),
        )
        .await?;
        txn.commit().await?;
        self.enforce(conference_id, !sessions.is_empty()).await;
        Ok(())
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
        // Part H §47: the interactive stage stays small (audience = VIEWER).
        let profile = crate::services::profiles::MeetingProfileResolver::load(
            &txn,
            &space,
            Some(&conference),
        )
        .await?;
        crate::services::specialized::check_stage_capacity(
            &txn,
            &profile,
            conference_id,
            &target,
            req.role,
        )
        .await?;
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
        // Part H §44: promotion to / demotion from the stage is explicit and audited.
        let promotion = match (previous == Role::Viewer, req.role == Role::Viewer) {
            (true, false) => Some((event::PARTICIPANT_PROMOTED, action::PARTICIPANT_PROMOTE)),
            (false, true) => Some((event::PARTICIPANT_DEMOTED, action::PARTICIPANT_DEMOTE)),
            _ => None,
        };
        if let Some((promotion_event, promotion_action)) = promotion {
            audit(
                &txn,
                &authority.audit,
                AuditRecord {
                    meeting_space_id: space.id,
                    conference_id: Some(conference_id),
                    action: promotion_action,
                    target_type: Some("PARTICIPANT"),
                    target_ref: Some(target.id.to_string()),
                    metadata: json!({ "from": previous, "to": req.role }),
                },
            )
            .await?;
            emit(&txn, participant_event(space.id, &updated, promotion_event)).await?;
            crate::services::ops_metrics::inc(if req.role == Role::Viewer {
                &crate::services::ops_metrics::OPS_METRICS.demotions
            } else {
                &crate::services::ops_metrics::OPS_METRICS.promotions
            });
        }
        // Part H: a role that loses backstage access leaves the backstage now.
        let updated = crate::services::specialized::clear_backstage_if_unauthorized(
            &txn,
            &conference,
            &updated,
            &pctx,
            &authority.audit,
        )
        .await?
        .unwrap_or(updated);
        // Part F: compare the effective projections (role ∧ policy ∧ locks).
        let collab_policy = crate::services::collaboration::effective_policy(&space, &conference);
        let restrictions = crate::services::collaboration::media::MediaRestrictions::of(&updated);
        let project = |role| {
            crate::services::collaboration::media::effective_media(
                &pctx,
                &collab_policy,
                role,
                restrictions,
            )
        };
        let (before, after) = (project(previous), project(req.role));
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
                        "permissions": after,
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
            external: None,
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
            inviter_user_id: Some(Uuid::new_v4()),
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
            screen_share_started_at: None,
            screen_share_track_ref: None,
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
