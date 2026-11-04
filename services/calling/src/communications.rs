//! Personal policy adapters. Calling retains its canonical calls and local block rules.
use crate::{
    calls::{
        app::AppCallService,
        command::CallScope,
        errors::{CallError, CallResult},
    },
    error::CallingError,
    internal_auth::middleware::InternalCaller,
    models::calls,
    state::AppState,
};
use axum::{extract::State, routing::post, Json, Router};
use cf_communications_policy::{Action, PolicyCheck, PolicyClient, PolicyError, Scope};
use cf_service_auth::{ApplicationEnvironmentKind, DelegatedPlatformContext, InternalPermission};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub fn scope(s: CallScope) -> Scope {
    Scope {
        tenant_id: s.tenant_id().as_uuid(),
        application_id: s.application_id().as_uuid(),
        environment_id: s.environment_id().as_uuid(),
    }
}
fn policy_error(e: PolicyError) -> CallError {
    match e {
        PolicyError::Denied => CallError::TargetUnavailable,
        PolicyError::Unavailable => CallError::CommunicationsPolicyUnavailable,
    }
}
impl AppCallService {
    pub(crate) async fn personal_call_governed(&self, call: &calls::Model) -> CallResult<bool> {
        if !self
            .communications
            .as_ref()
            .is_some_and(|c| c.governs(scope(crate::calls::app::scope_of(call))))
        {
            return Ok(false);
        }
        Ok(self
            .db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM enterprise_call_contexts WHERE call_id=$1",
                [call.id.into()],
            ))
            .await?
            .is_none())
    }

    pub fn with_communications(mut self, client: Option<PolicyClient>) -> Self {
        self.communications = client;
        self
    }
    pub fn personal_available(&self) -> bool {
        self.config.new_calls_enabled && self.config.app_calling_enabled && self.media.is_some()
    }
    pub(crate) async fn require_personal_create(
        &self,
        ctx: &crate::platform::context::PlatformAuthContext,
        actor: Uuid,
        targets: &[Uuid],
    ) -> CallResult<()> {
        let Some(client) = &self.communications else {
            return Ok(());
        };
        let own = CallScope::of_platform(ctx);
        if !client.governs(scope(own)) {
            return Ok(());
        }
        let delegated = crate::internal_auth::delegation::delegated_context(ctx, None)
            .map_err(|_| CallError::TargetUnavailable)?;
        for target in targets {
            client
                .check(
                    delegated.clone(),
                    &PolicyCheck {
                        action: Action::DirectCall,
                        actor_principal_id: actor,
                        target_principal_id: *target,
                        credential_issued_at: None,
                        credential_principal_id: None,
                    },
                )
                .await
                .map_err(policy_error)?;
        }
        Ok(())
    }
    /// Canonical enterprise context, not client JSON, determines the exception. The
    /// enterprise creation route already required enterprise.calls.use in this scope.
    pub(crate) async fn require_personal_call(
        &self,
        call: &calls::Model,
        evidence: Option<(Uuid, i64)>,
    ) -> CallResult<()> {
        let Some(client) = &self.communications else {
            return Ok(());
        };
        let own = crate::calls::app::scope_of(call);
        if !client.governs(scope(own)) {
            return Ok(());
        }
        let enterprise = self
            .db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM enterprise_call_contexts WHERE call_id=$1",
                [call.id.into()],
            ))
            .await?;
        if enterprise.is_some() {
            return Ok(());
        }
        let row = self
            .db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT kind::text AS kind FROM platform_environments WHERE id=$1",
                [call.environment_id.into()],
            ))
            .await?
            .ok_or(CallError::CommunicationsPolicyUnavailable)?;
        let kind: String = row.try_get("", "kind")?;
        let kind = match kind.as_str() {
            "PRODUCTION" => ApplicationEnvironmentKind::Production,
            "TEST" => ApplicationEnvironmentKind::Test,
            "SANDBOX" => ApplicationEnvironmentKind::Sandbox,
            _ => return Err(CallError::CommunicationsPolicyUnavailable),
        };
        let delegated = DelegatedPlatformContext::new(
            call.tenant_id,
            call.application_id,
            call.environment_id,
            kind,
            call.created_by_service_account_id,
        );
        let people=self.db().query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT principal_id,role FROM call_participants WHERE call_id=$1 AND ended_at IS NULL",[call.id.into()])).await?;
        let mut actor = None;
        let mut targets = Vec::new();
        for p in people {
            let role: String = p.try_get("", "role")?;
            let id: Uuid = p.try_get("", "principal_id")?;
            if role == "INITIATOR" {
                actor = Some(id);
            } else {
                targets.push(id);
            }
        }
        let actor = actor.ok_or(CallError::TargetUnavailable)?;
        for target in targets {
            client
                .check(
                    delegated.clone(),
                    &PolicyCheck {
                        action: Action::DirectCall,
                        actor_principal_id: actor,
                        target_principal_id: target,
                        credential_issued_at: evidence.map(|x| x.1),
                        credential_principal_id: evidence.map(|x| x.0),
                    },
                )
                .await
                .map_err(policy_error)?;
        }
        Ok(())
    }
}
impl AppState {
    pub(crate) async fn require_personal_signaling(
        &self,
        identity: &crate::signaling::SignalingIdentity,
    ) -> Result<(), CallingError> {
        if !self
            .communications
            .as_ref()
            .is_some_and(|c| c.governs(scope(identity.scope())))
        {
            return Ok(());
        }
        let own = scope(identity.scope());
        let row=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT 1 FROM calling_communication_revocations WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4 AND credentials_not_before>=$5",[own.tenant_id.into(),own.application_id.into(),own.environment_id.into(),identity.principal_id().as_uuid().into(),identity.issued_at().timestamp().into()])).await.map_err(|_|CallingError::ServiceUnavailable)?;
        if row.is_some() {
            return Err(CallingError::AuthenticationFailed);
        }
        Ok(())
    }
    pub fn with_communications(mut self, client: Option<PolicyClient>) -> Self {
        self.app = Arc::new((*self.app).clone().with_communications(client.clone()));
        self.communications = client;
        self
    }
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/communications/blocks/check", post(blocks))
        .route("/communications/capabilities", post(capabilities))
        .route("/communications/events", post(event))
}
fn own(
    state: &AppState,
    caller: &InternalCaller,
    permission: InternalPermission,
) -> Result<DelegatedPlatformContext, CallingError> {
    caller.require_permission(permission)?;
    let d = caller
        .context()
        .delegated_platform_context()
        .cloned()
        .ok_or(CallingError::InternalPermissionDenied)?;
    if !state
        .communications
        .as_ref()
        .is_some_and(|p| p.governs(Scope::of(&d)))
    {
        return Err(CallingError::NotFound);
    }
    Ok(d)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pair {
    actor_principal_id: Uuid,
    target_principal_id: Uuid,
}
async fn blocks(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(pair): Json<Pair>,
) -> Result<Json<Value>, CallingError> {
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsRead,
    )?;
    let blocked = !crate::calls::app::blocks::blocked_targets(
        &state.db,
        d.application_environment_id(),
        pair.actor_principal_id,
        &[pair.target_principal_id],
    )
    .await?
    .is_empty();
    Ok(Json(json!({"blocked":blocked})))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Principal {
    principal_id: Uuid,
}
async fn capabilities(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(principal): Json<Principal>,
) -> Result<Json<Value>, CallingError> {
    own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsRead,
    )?;
    if principal.principal_id.is_nil() {
        return Err(CallingError::Validation("Invalid principal"));
    }
    let available = state.app.personal_available();
    Ok(Json(
        json!({"available":available,"reason":if available {"AVAILABLE"} else {"DISABLED"}}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Event {
    schema_version: u32,
    event_id: Uuid,
    aggregate_id: Uuid,
    version: i64,
    #[serde(rename = "type")]
    kind: String,
    data: Value,
}
async fn event(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(event): Json<Event>,
) -> Result<Json<Value>, CallingError> {
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsApply,
    )?;
    if event.schema_version != 1
        || event.version < 1
        || !event.kind.starts_with("communications.")
        || event.kind.len() > 80
    {
        return Err(CallingError::Validation("Invalid Communications event"));
    }
    let digest = {
        use sha2::{Digest, Sha256};
        Sha256::digest(serde_json::to_vec(&json!({"aggregateId":event.aggregate_id,"version":event.version,"type":event.kind,"data":event.data})).map_err(|_|CallingError::Internal)?).to_vec()
    };
    let txn = state
        .db
        .begin()
        .await
        .map_err(|_| CallingError::ServiceUnavailable)?;
    let n=txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO calling_communication_inbox(event_id,tenant_id,application_id,environment_id,request_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",[event.event_id.into(),d.tenant_id().into(),d.application_id().into(),d.application_environment_id().into(),digest.clone().into()])).await.map_err(|_|CallingError::ServiceUnavailable)?.rows_affected();
    if n == 0 {
        let row=txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT request_digest FROM calling_communication_inbox WHERE event_id=$1 AND tenant_id=$2 AND application_id=$3 AND environment_id=$4",[event.event_id.into(),d.tenant_id().into(),d.application_id().into(),d.application_environment_id().into()])).await.map_err(|_|CallingError::ServiceUnavailable)?.ok_or(CallingError::NotFound)?;
        if row
            .try_get::<Vec<u8>>("", "request_digest")
            .map_err(|_| CallingError::Internal)?
            != digest
        {
            return Err(CallingError::Validation("Event identity conflict"));
        }
    }
    if let (Some(principal), Some(cutoff)) = (
        event.data["principalId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok()),
        event.data["credentialsNotBefore"].as_i64(),
    ) {
        txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO calling_communication_revocations(tenant_id,application_id,environment_id,principal_id,credentials_not_before) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,application_id,environment_id,principal_id) DO UPDATE SET credentials_not_before=GREATEST(calling_communication_revocations.credentials_not_before,$5),updated_at=now()",[d.tenant_id().into(),d.application_id().into(),d.application_environment_id().into(),principal.into(),cutoff.into()])).await.map_err(|_|CallingError::ServiceUnavailable)?;
    }
    txn.commit()
        .await
        .map_err(|_| CallingError::ServiceUnavailable)?;
    // No event grants call permissions. Every new creation, answer, credential and
    // media admission checks live Hub policy; safety operations remain available.
    Ok(Json(json!({"applied":n==1,"duplicate":n==0})))
}
