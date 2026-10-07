//! Endpoint registry operations.

use chrono::Duration;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
    TransactionTrait,
};
use uuid::Uuid;

use super::{
    CallEndpoint, CallEndpointId, EndpointCapabilities, EndpointKey, EndpointKind, EndpointStatus,
    PresenceState,
};
use crate::{
    calls::{
        command::CallScope,
        errors::{CallError, CallResult},
    },
    domain::{PrincipalId, SharedClock},
    models::call_endpoints,
    platform::{context::PlatformAuthContext, principals::ResolvedPrincipal},
};

/// Registration input (scope and principal come from authentication / resolution).
#[derive(Debug, Clone)]
pub struct NewEndpoint {
    pub key: EndpointKey,
    pub kind: EndpointKind,
    pub capabilities: EndpointCapabilities,
    pub push_target_ref: Option<Uuid>,
}

/// How long after its last signal an ONLINE endpoint still counts as online when
/// ordering ring targets (a hint only).
#[derive(Debug, Clone, Copy)]
pub struct PresenceWindow(pub Duration);

impl Default for PresenceWindow {
    fn default() -> Self {
        Self(Duration::seconds(90))
    }
}

#[derive(Clone)]
pub struct EndpointService {
    db: DatabaseConnection,
    clock: SharedClock,
    presence_window: PresenceWindow,
}

fn scoped(scope: CallScope) -> sea_orm::Select<call_endpoints::Entity> {
    call_endpoints::Entity::find()
        .filter(call_endpoints::Column::TenantId.eq(scope.tenant_id().as_uuid()))
        .filter(call_endpoints::Column::ApplicationId.eq(scope.application_id().as_uuid()))
        .filter(call_endpoints::Column::EnvironmentId.eq(scope.environment_id().as_uuid()))
}

impl EndpointService {
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        presence_window: PresenceWindow,
    ) -> Self {
        Self {
            db,
            clock,
            presence_window,
        }
    }

    /// Registers an endpoint for a principal resolved in the caller's own scope.
    /// Idempotent for the same principal and key (capabilities and push reference are
    /// refreshed); a key held by another principal is a conflict that reveals nothing
    /// about that principal. Returns `(endpoint, created)`.
    pub async fn register(
        &self,
        ctx: &PlatformAuthContext,
        principal: &ResolvedPrincipal,
        new: NewEndpoint,
    ) -> CallResult<(CallEndpoint, bool)> {
        let scope = CallScope::of_platform(ctx);
        if CallScope::of_principal(principal) != scope {
            return Err(CallError::NotFound);
        }
        if !new.capabilities.audio && !new.capabilities.video {
            return Err(CallError::Validation(
                "An endpoint must support audio or video.",
            ));
        }
        let txn = self.db.begin().await?;
        // Serialize registrations of one key in one application environment.
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            vec![format!(
                "cfmeet.calling.endpoint-key:{}:{}:{}:{}",
                scope.tenant_id(),
                scope.application_id(),
                scope.environment_id(),
                new.key.as_str()
            )
            .into()],
        ))
        .await?;
        let now = self.clock.now();
        if let Some(existing) = scoped(scope)
            .filter(call_endpoints::Column::EndpointKey.eq(new.key.as_str()))
            .filter(call_endpoints::Column::Status.eq(EndpointStatus::Active))
            .one(&txn)
            .await?
        {
            if existing.principal_id != principal.id().as_uuid()
                || existing.endpoint_kind != new.kind
            {
                return Err(CallError::Conflict(
                    "the endpoint key is registered to another endpoint",
                ));
            }
            let at = now.max(existing.updated_at);
            let mut active: call_endpoints::ActiveModel = existing.into();
            active.supports_audio = Set(new.capabilities.audio);
            active.supports_video = Set(new.capabilities.video);
            active.push_target_ref = Set(new.push_target_ref);
            active.updated_at = Set(at);
            let endpoint = active.update(&txn).await?;
            txn.commit().await?;
            return Ok((endpoint.into(), false));
        }
        let endpoint = call_endpoints::ActiveModel {
            id: Set(CallEndpointId::generate().as_uuid()),
            tenant_id: Set(scope.tenant_id().as_uuid()),
            application_id: Set(scope.application_id().as_uuid()),
            environment_id: Set(scope.environment_id().as_uuid()),
            principal_id: Set(principal.id().as_uuid()),
            endpoint_key: Set(new.key.as_str().to_string()),
            endpoint_kind: Set(new.kind),
            supports_audio: Set(new.capabilities.audio),
            supports_video: Set(new.capabilities.video),
            status: Set(EndpointStatus::Active),
            presence_state: Set(PresenceState::Offline),
            last_seen_at: Set(None),
            push_target_ref: Set(new.push_target_ref),
            registered_by_service_account_id: Set(ctx.service_account_id().as_uuid()),
            registered_by_credential_id: Set(ctx.credential_id().as_uuid()),
            created_at: Set(now),
            updated_at: Set(now),
            revoked_at: Set(None),
        }
        .insert(&txn)
        .await?;
        txn.commit().await?;
        tracing::info!(
            endpoint_id = %endpoint.id,
            principal_id = %endpoint.principal_id,
            endpoint_kind = %endpoint.endpoint_kind,
            "call endpoint registered"
        );
        Ok((endpoint.into(), true))
    }

    /// Endpoints of one principal in `scope`, newest first.
    pub async fn list(
        &self,
        scope: CallScope,
        principal_id: PrincipalId,
        include_revoked: bool,
    ) -> CallResult<Vec<CallEndpoint>> {
        let mut query =
            scoped(scope).filter(call_endpoints::Column::PrincipalId.eq(principal_id.as_uuid()));
        if !include_revoked {
            query = query.filter(call_endpoints::Column::Status.eq(EndpointStatus::Active));
        }
        Ok(query
            .order_by_desc(call_endpoints::Column::CreatedAt)
            .order_by_desc(call_endpoints::Column::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// One endpoint of `scope` (foreign endpoints are not found).
    pub async fn get(&self, scope: CallScope, id: CallEndpointId) -> CallResult<CallEndpoint> {
        Ok(scoped(scope)
            .filter(call_endpoints::Column::Id.eq(id.as_uuid()))
            .one(&self.db)
            .await?
            .ok_or(CallError::NotFound)?
            .into())
    }

    /// Revokes an endpoint (idempotent). Returns `(endpoint, newly_revoked)`.
    pub async fn revoke(
        &self,
        scope: CallScope,
        id: CallEndpointId,
    ) -> CallResult<(CallEndpoint, bool)> {
        let txn = self.db.begin().await?;
        let endpoint = scoped(scope)
            .filter(call_endpoints::Column::Id.eq(id.as_uuid()))
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(CallError::NotFound)?;
        if endpoint.status == EndpointStatus::Revoked {
            txn.commit().await?;
            return Ok((endpoint.into(), false));
        }
        let at = self.clock.now().max(endpoint.updated_at);
        let mut active: call_endpoints::ActiveModel = endpoint.into();
        active.status = Set(EndpointStatus::Revoked);
        active.revoked_at = Set(Some(at));
        active.presence_state = Set(PresenceState::Offline);
        active.updated_at = Set(at);
        let endpoint = active.update(&txn).await?;
        txn.commit().await?;
        tracing::info!(endpoint_id = %endpoint.id, "call endpoint revoked");
        Ok((endpoint.into(), true))
    }

    /// The ACTIVE endpoint `id` of `principal_id` in `scope`, or not found.
    pub async fn active_endpoint(
        &self,
        scope: CallScope,
        principal_id: PrincipalId,
        id: CallEndpointId,
    ) -> CallResult<CallEndpoint> {
        Ok(scoped(scope)
            .filter(call_endpoints::Column::Id.eq(id.as_uuid()))
            .filter(call_endpoints::Column::PrincipalId.eq(principal_id.as_uuid()))
            .filter(call_endpoints::Column::Status.eq(EndpointStatus::Active))
            .one(&self.db)
            .await?
            .ok_or(CallError::NotFound)?
            .into())
    }

    /// Records a presence signal of an ACTIVE endpoint (routing hint only).
    pub async fn record_presence(
        &self,
        id: CallEndpointId,
        presence: PresenceState,
    ) -> CallResult<()> {
        let now = self.clock.now();
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE call_endpoints \
                    SET presence_state = $2, \
                        last_seen_at = GREATEST($3, created_at, COALESCE(last_seen_at, created_at)), \
                        updated_at = GREATEST($3, updated_at) \
                  WHERE id = $1 AND status = 'ACTIVE'",
                vec![id.as_uuid().into(), presence.as_str().into(), now.into()],
            ))
            .await?;
        Ok(())
    }

    /// The endpoints a principal may be rung on (every application Call needs audio; video
    /// is optional per endpoint), in routing order:
    /// recently-online first, then most recently seen, then oldest registration; at most
    /// `cap`. Presence only orders the list — every candidate is an ACTIVE endpoint of the
    /// principal itself, in the Call's own scope.
    pub(crate) async fn eligible_in(
        &self,
        txn: &DatabaseTransaction,
        scope: CallScope,
        principal_id: Uuid,
        cap: usize,
    ) -> CallResult<Vec<call_endpoints::Model>> {
        let now = self.clock.now();
        let online_since = now - self.presence_window.0;
        let mut candidates: Vec<call_endpoints::Model> = scoped(scope)
            .filter(call_endpoints::Column::PrincipalId.eq(principal_id))
            .filter(call_endpoints::Column::Status.eq(EndpointStatus::Active))
            .filter(call_endpoints::Column::SupportsAudio.eq(true))
            .all(txn)
            .await?;
        candidates.sort_by(|a, b| {
            let online = |e: &call_endpoints::Model| {
                e.presence_state == PresenceState::Online
                    && e.last_seen_at.is_some_and(|t| t >= online_since)
            };
            online(b)
                .cmp(&online(a))
                .then(b.last_seen_at.cmp(&a.last_seen_at))
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        candidates.truncate(cap);
        Ok(candidates)
    }
}
