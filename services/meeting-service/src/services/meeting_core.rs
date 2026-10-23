//! Shared dependencies and authoritative loaders for the Phase 2 meeting services.
//!
//! Every service holds an `Arc<DomainCore>`; there is no global business state.

use std::{collections::HashMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use sea_orm::{
    sea_query::LockType, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter, QuerySelect, SqlErr,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    config::{FeatureFlags, MeetingV2Config},
    domain::{
        AdmissionState, MemberStatus, PrincipalType, RecordOrigin, Role, RoleSource, SpaceStatus,
    },
    error::{ApiError, ApiResult, ErrorCode},
    models::{conference_participants, conference_sessions, meeting_members, meeting_spaces},
    services::{
        actor::{
            ActorContext, ActorKind, PlatformActor, RegisteredActor, SharedOrganizationDirectory,
        },
        meeting_events::AuditActor,
        rtc_provider::RtcProvider,
        secrets, RedisService,
    },
};

// ---------------------------------------------------------------------------
// Extension points
// ---------------------------------------------------------------------------

/// Input to an external access adapter (HEALTHCARE_APPOINTMENT / CUSTOM).
#[derive(Debug, Clone)]
pub struct AccessAdapterRequest {
    pub meeting_space_id: Uuid,
    pub conference_id: Option<Uuid>,
    pub adapter_ref: String,
    pub user_id: Option<Uuid>,
    pub verified_email: Option<String>,
    pub organizations: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterDecision {
    Allow { role: Option<Role> },
    Deny,
}

/// Server-side policy adapter. Generic meeting code never embeds HealthNet logic;
/// a HealthNet appointment adapter is registered by the deployment.
#[async_trait]
pub trait ExternalAccessAdapter: Send + Sync {
    async fn evaluate(&self, request: &AccessAdapterRequest) -> AdapterDecision;
}

/// CUSTOM waiting-room evaluator. `true` = actor must wait.
#[async_trait]
pub trait WaitingRoomEvaluator: Send + Sync {
    async fn must_wait(&self, meeting_space_id: Uuid, user_id: Option<Uuid>, role: Role) -> bool;
}

#[derive(Default, Clone)]
pub struct AccessAdapterRegistry {
    adapters: HashMap<String, Arc<dyn ExternalAccessAdapter>>,
    pub waiting_room: HashMap<String, Arc<dyn WaitingRoomEvaluator>>,
}

impl AccessAdapterRegistry {
    pub fn register(&mut self, name: impl Into<String>, adapter: Arc<dyn ExternalAccessAdapter>) {
        self.adapters.insert(name.into(), adapter);
    }

    pub fn register_waiting_room(
        &mut self,
        name: impl Into<String>,
        evaluator: Arc<dyn WaitingRoomEvaluator>,
    ) {
        self.waiting_room.insert(name.into(), evaluator);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ExternalAccessAdapter>> {
        self.adapters.get(name).cloned()
    }
}

/// Failed-attempt limiter for passcodes. Uses Redis when available and falls back to
/// an in-process window so Redis loss never disables brute-force protection.
pub struct AttemptLimiter {
    redis: Option<Arc<RedisService>>,
    local: Mutex<HashMap<String, (i64, Instant)>>,
    max_attempts: i64,
    window_secs: u64,
}

impl AttemptLimiter {
    pub fn new(redis: Option<Arc<RedisService>>, max_attempts: i64, window_secs: u64) -> Self {
        Self {
            redis,
            local: Mutex::new(HashMap::new()),
            max_attempts,
            window_secs,
        }
    }

    async fn local_count(&self, key: &str, increment: bool) -> i64 {
        let mut map = self.local.lock().await;
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_insert((0, now));
        if now.duration_since(entry.1).as_secs() >= self.window_secs {
            *entry = (0, now);
        }
        if increment {
            entry.0 += 1;
        }
        entry.0
    }

    /// Whether `key` has reached `multiplier` × the configured failure budget.
    pub async fn is_blocked(&self, key: &str, multiplier: i64) -> bool {
        let max = self.max_attempts * multiplier.max(1);
        let redis_key = format!("rate:meeting_passcode:{key}");
        if let Some(redis) = &self.redis {
            match redis.get(&redis_key).await {
                Ok(Some(v)) => return v.parse::<i64>().unwrap_or(0) >= max,
                Ok(None) => return self.local_count(key, false).await >= max,
                Err(_) => {}
            }
        }
        self.local_count(key, false).await >= max
    }

    /// Record one failure.
    pub async fn record_failure(&self, key: &str) {
        let redis_key = format!("rate:meeting_passcode:{key}");
        if let Some(redis) = &self.redis {
            if redis
                .incr_with_ttl(&redis_key, self.window_secs)
                .await
                .is_ok()
            {
                return;
            }
            tracing::warn!("passcode limiter: Redis unavailable, using in-process window");
        }
        self.local_count(key, true).await;
    }
}

// ---------------------------------------------------------------------------
// DomainCore
// ---------------------------------------------------------------------------

pub struct DomainCore {
    pub db: DatabaseConnection,
    pub flags: FeatureFlags,
    pub config: MeetingV2Config,
    pub organizations: SharedOrganizationDirectory,
    pub rtc: Arc<dyn RtcProvider>,
    pub adapters: AccessAdapterRegistry,
    pub appointments:
        Arc<dyn crate::services::appointment_authorization::AppointmentAuthorizationAdapter>,
    pub passcode_limiter: AttemptLimiter,
    /// Part F: realtime fanout for transient events (reactions). Never authority.
    pub redis: Option<Arc<RedisService>>,
    /// Part F: participant-scoped collaboration rate limits.
    pub collab_limiter: crate::services::collaboration::CollabLimiter,
    /// Part G provider backends (recording, storage, transcription, intelligence).
    pub artifacts: crate::services::artifacts::ArtifactBackends,
    /// Part I vendor-neutral telemetry sink (failures never affect meetings).
    pub telemetry: Arc<dyn crate::services::quality::sink::QualityTelemetrySink>,
    /// Part V H: Unified Messaging integration (`None` = legacy chat only).
    pub unified: Option<Arc<crate::services::unified_messaging::UnifiedMessaging>>,
}

pub type Core = Arc<DomainCore>;

/// Who a new meeting space belongs to (exactly one kind; see
/// `m20261101_000001_platform_environment_meetings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceOwner {
    /// A CF Meet account.
    User(Uuid),
    /// A Platform Access application environment, created by `service_account_id`.
    Environment {
        tenant_id: Uuid,
        application_id: Uuid,
        environment_id: Uuid,
        service_account_id: Uuid,
    },
}

impl SpaceOwner {
    pub fn user_id(&self) -> Option<Uuid> {
        match self {
            SpaceOwner::User(u) => Some(*u),
            SpaceOwner::Environment { .. } => None,
        }
    }
}

impl DomainCore {
    /// The owner of a space the actor creates, after the v2 write gate. A machine caller
    /// writes only when v2 writes are not restricted to an account cohort.
    pub fn require_v2_writer(&self, ctx: &ActorContext) -> ApiResult<SpaceOwner> {
        if let Some(p) = ctx.platform() {
            if self.flags.v2_write_allowlist.is_some() {
                return Err(ApiError::domain(
                    ErrorCode::FeatureDisabled,
                    "Meeting v2 writes are restricted to an account cohort",
                ));
            }
            return Ok(SpaceOwner::Environment {
                tenant_id: p.tenant_id,
                application_id: p.application_id,
                environment_id: p.environment_id,
                service_account_id: p.service_account_id,
            });
        }
        let actor = ctx.require_registered()?;
        self.require_v2_write(actor)?;
        Ok(SpaceOwner::User(actor.user_id))
    }

    pub fn require_v2_write(&self, actor: &RegisteredActor) -> ApiResult<()> {
        if self.flags.may_write_v2(actor.user_id) {
            Ok(())
        } else {
            Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Meeting v2 is not enabled for this account",
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Errors / helpers
// ---------------------------------------------------------------------------

pub fn not_found_space() -> ApiError {
    ApiError::domain(ErrorCode::MeetingNotFound, "Meeting not found")
}

pub fn not_found_conference() -> ApiError {
    ApiError::domain(ErrorCode::ConferenceNotFound, "Conference not found")
}

pub fn not_found_participant() -> ApiError {
    ApiError::domain(
        ErrorCode::ParticipantNotFound,
        "Participant not found in this conference",
    )
}

pub fn permission_denied(message: &str) -> ApiError {
    ApiError::domain(ErrorCode::PermissionDenied, message)
}

pub fn state_conflict(message: impl Into<String>) -> ApiError {
    ApiError::domain(ErrorCode::ConferenceStateConflict, message)
}

pub fn validation(message: impl Into<String>) -> ApiError {
    ApiError::domain(ErrorCode::ValidationError, message)
}

pub fn is_unique_violation(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

/// A legacy-origin space whose legacy meeting was deleted behaves as deleted.
pub fn space_is_visible(space: &meeting_spaces::Model) -> bool {
    space.status != SpaceStatus::Deleted
        && !(space.origin == RecordOrigin::Legacy && space.legacy_meeting_id.is_none())
}

pub async fn find_space<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
) -> ApiResult<meeting_spaces::Model> {
    meeting_spaces::Entity::find_by_id(id)
        .one(conn)
        .await?
        .filter(space_is_visible)
        .ok_or_else(not_found_space)
}

pub async fn lock_space<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
) -> ApiResult<meeting_spaces::Model> {
    meeting_spaces::Entity::find_by_id(id)
        .lock(LockType::Update)
        .one(conn)
        .await?
        .filter(space_is_visible)
        .ok_or_else(not_found_space)
}

pub async fn find_conference<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
) -> ApiResult<conference_sessions::Model> {
    conference_sessions::Entity::find_by_id(id)
        .one(conn)
        .await?
        .ok_or_else(not_found_conference)
}

/// `SELECT … FOR UPDATE` on the conference row. Lock order everywhere:
/// conference → participant(s) → invitation.
pub async fn lock_conference<C: ConnectionTrait>(
    conn: &C,
    id: Uuid,
) -> ApiResult<conference_sessions::Model> {
    conference_sessions::Entity::find_by_id(id)
        .lock(LockType::Update)
        .one(conn)
        .await?
        .ok_or_else(not_found_conference)
}

/// Load a participant strictly scoped to `conference_id` (cross-conference IDs are
/// indistinguishable from unknown IDs).
pub async fn find_scoped_participant<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    participant_id: Uuid,
    for_update: bool,
) -> ApiResult<conference_participants::Model> {
    let mut q = conference_participants::Entity::find_by_id(participant_id)
        .filter(conference_participants::Column::ConferenceId.eq(conference_id));
    if for_update {
        q = q.lock(LockType::Update);
    }
    q.one(conn).await?.ok_or_else(not_found_participant)
}

/// Active persistent membership for a registered actor (by user, or by verified email).
pub async fn active_membership<C: ConnectionTrait>(
    conn: &C,
    space_id: Uuid,
    actor: &RegisteredActor,
) -> ApiResult<Option<meeting_members::Model>> {
    let mut cond = sea_orm::Condition::any().add(meeting_members::Column::UserId.eq(actor.user_id));
    if let Some(email) = &actor.verified_email {
        cond = cond.add(
            sea_orm::Condition::all()
                .add(meeting_members::Column::PrincipalType.eq(PrincipalType::Email))
                .add(meeting_members::Column::PrincipalRef.eq(email.to_ascii_lowercase())),
        );
    }
    let members = meeting_members::Entity::find()
        .filter(meeting_members::Column::MeetingSpaceId.eq(space_id))
        .filter(meeting_members::Column::Status.eq(MemberStatus::Active))
        .filter(cond)
        .all(conn)
        .await?;
    Ok(members.into_iter().max_by_key(|m| m.role.rank()))
}

/// Spaces whose security profile (or explicit configuration) requires that an
/// access token's revocation status was actually verified. Per-token revocation lives
/// only in Redis; when it cannot be checked these spaces fail closed. Redis is never
/// meeting-state authority — this only decides whether an *authenticated identity*
/// is trustworthy enough for a strict space.
pub fn requires_verified_revocation(space: &meeting_spaces::Model) -> bool {
    matches!(
        space.security_profile,
        crate::domain::SecurityProfile::Healthcare | crate::domain::SecurityProfile::HighSecurity
    ) || crate::services::meeting_space::access_config(space).require_verified_revocation
}

pub fn enforce_revocation_policy(
    space: &meeting_spaces::Model,
    actor: &RegisteredActor,
) -> ApiResult<()> {
    if !actor.revocation_checked && requires_verified_revocation(space) {
        return Err(ApiError::domain(
            ErrorCode::RevocationCheckUnavailable,
            "Your session cannot be verified right now. Please try again shortly.",
        ));
    }
    Ok(())
}

/// Whether `space` is owned by the platform actor's application environment (the whole
/// live chain: tenant, application AND environment). User-owned spaces never are.
pub fn platform_owns(space: &meeting_spaces::Model, actor: &PlatformActor) -> bool {
    space.owner_user_id.is_none()
        && space.platform_environment_id == Some(actor.environment_id)
        && space.platform_application_id == Some(actor.application_id)
        && space.platform_tenant_id == Some(actor.tenant_id)
}

/// Persistent (space-level) role of any actor: a registered user's owner / membership
/// role, OWNER for the machine caller of the owning environment, nothing otherwise.
pub async fn actor_space_role<C: ConnectionTrait>(
    conn: &C,
    space: &meeting_spaces::Model,
    actor: &ActorContext,
) -> ApiResult<Option<(Role, RoleSource)>> {
    match &actor.kind {
        ActorKind::RegisteredUser(r) => space_role(conn, space, r).await,
        ActorKind::Platform(p) if platform_owns(space, p) => {
            Ok(Some((Role::Owner, RoleSource::Owner)))
        }
        _ => Ok(None),
    }
}

/// Persistent (space-level) role of a registered actor: owner, or membership role.
pub async fn space_role<C: ConnectionTrait>(
    conn: &C,
    space: &meeting_spaces::Model,
    actor: &RegisteredActor,
) -> ApiResult<Option<(Role, RoleSource)>> {
    enforce_revocation_policy(space, actor)?;
    if space.owner_user_id == Some(actor.user_id) {
        return Ok(Some((Role::Owner, RoleSource::Owner)));
    }
    Ok(active_membership(conn, space.id, actor)
        .await?
        .map(|m| (m.role, RoleSource::Membership)))
}

/// Resolve a participant credential (guest / invited external) within one conference.
pub async fn participant_by_credential<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    credential: &str,
    for_update: bool,
) -> ApiResult<Option<conference_participants::Model>> {
    let mut q = conference_participants::Entity::find()
        .filter(conference_participants::Column::ConferenceId.eq(conference_id))
        .filter(
            conference_participants::Column::CredentialHash.eq(secrets::hash_secret(credential)),
        );
    if for_update {
        q = q.lock(LockType::Update);
    }
    Ok(q.one(conn).await?)
}

/// The actor's own participant row in a conference, if any.
pub async fn own_participant<C: ConnectionTrait>(
    conn: &C,
    conference_id: Uuid,
    actor: &ActorContext,
    for_update: bool,
) -> ApiResult<Option<conference_participants::Model>> {
    match &actor.kind {
        ActorKind::RegisteredUser(r) => {
            let mut q = conference_participants::Entity::find()
                .filter(conference_participants::Column::ConferenceId.eq(conference_id))
                .filter(conference_participants::Column::UserId.eq(r.user_id));
            if for_update {
                q = q.lock(LockType::Update);
            }
            Ok(q.one(conn).await?)
        }
        ActorKind::ParticipantCredential(c) => {
            participant_by_credential(conn, conference_id, c, for_update).await
        }
        _ => Ok(None),
    }
}

/// Authority of an actor over a live conference.
#[derive(Debug, Clone)]
pub struct ConferenceAuthority {
    pub role: Role,
    pub participant: Option<conference_participants::Model>,
    pub audit: AuditActor,
}

/// Effective management role: the stronger of the space-level role (owner/member)
/// and the actor's ADMITTED participant role. A REMOVED/BANNED/DENIED participant has
/// no authority at all, whatever stale client state it holds.
pub async fn conference_authority<C: ConnectionTrait>(
    conn: &C,
    space: &meeting_spaces::Model,
    conference_id: Uuid,
    actor: &ActorContext,
) -> ApiResult<Option<ConferenceAuthority>> {
    if let ActorKind::System(name) = actor.kind {
        return Ok(Some(ConferenceAuthority {
            role: Role::Owner,
            participant: None,
            audit: AuditActor::system(name),
        }));
    }
    // A machine caller manages only its own environment's meetings (owner authority, no
    // participant row); any other meeting is as if it did not exist for it.
    if let ActorKind::Platform(p) = &actor.kind {
        return Ok(platform_owns(space, p).then(|| ConferenceAuthority {
            role: Role::Owner,
            participant: None,
            audit: AuditActor::from_ctx(actor),
        }));
    }
    let participant = own_participant(conn, conference_id, actor, false).await?;
    if let Some(p) = &participant {
        if matches!(
            p.admission_state,
            AdmissionState::Removed | AdmissionState::Banned | AdmissionState::Denied
        ) {
            return Ok(None);
        }
    }
    let participant_role = participant
        .as_ref()
        .filter(|p| p.admission_state == AdmissionState::Admitted)
        .map(|p| p.role);
    let persistent = match actor.registered() {
        Some(r) => space_role(conn, space, r).await?.map(|(role, _)| role),
        None => None,
    };
    let role = match (participant_role, persistent) {
        (Some(a), Some(b)) => Role::max(a, b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return Ok(None),
    };
    let audit = match (&participant, actor.registered()) {
        (_, Some(_)) => AuditActor::from_ctx(actor),
        (Some(p), None) => AuditActor::participant(p.id),
        (None, None) => AuditActor::from_ctx(actor),
    };
    Ok(Some(ConferenceAuthority {
        role,
        participant,
        audit,
    }))
}
