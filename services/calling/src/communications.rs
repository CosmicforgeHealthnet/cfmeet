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
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, State},
    routing::post,
    Json, Router,
};
use cf_communications_policy::{Action, PolicyCheck, PolicyClient, PolicyError, Scope};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
    InternalPermission,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
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
        // Sections 9–23 owner interfaces (CL-1 … CL-5): the Hub's facades read and act on
        // canonical Calling state for the delegated user; nothing is copied.
        .route(
            "/communications/calls",
            post(launch).layer(DefaultBodyLimit::max(OWNER_BODY_LIMIT)),
        )
        .route(
            "/communications/calls/participation",
            post(participation).layer(DefaultBodyLimit::max(OWNER_BODY_LIMIT)),
        )
        .route(
            "/communications/calls/history",
            post(history).layer(DefaultBodyLimit::max(OWNER_BODY_LIMIT)),
        )
        .route(
            "/communications/endpoints/verify",
            post(verify_endpoint).layer(DefaultBodyLimit::max(OWNER_BODY_LIMIT)),
        )
        .route(
            "/communications/endpoints/revoke",
            post(revoke_endpoint).layer(DefaultBodyLimit::max(OWNER_BODY_LIMIT)),
        )
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

// ---------------------------------------------------------------------------
// Sections 9–23 owner interfaces (CL-1 … CL-5). Common rules: `own()` (permission,
// delegated context, governed personal scope → else 403 / 403 / 404), then the body's
// principal must be the delegated NATIVE_CF_USER (else 403). Each route runs the
// canonical domain code of its customer counterpart; responses carry no credential,
// media, provider or contact data.
// ---------------------------------------------------------------------------

/// Request bodies of the owner interfaces are small, closed objects.
const OWNER_BODY_LIMIT: usize = 16 * 1024;
/// Hub idempotency keys: 1–200 printable ASCII characters.
const MAX_HUB_KEY_LEN: usize = 200;
/// The customer route's bound on targets, checked before any policy call (the domain
/// then enforces `CALLING_MAX_GROUP_PARTICIPANTS`, distinctness and "no self").
const MAX_LAUNCH_TARGETS: usize = 64;
/// CL-2 reads at most this many Calls.
const MAX_PARTICIPATION_CALLS: usize = 50;
/// Namespace of Hub-launched creation keys inside Calling's per-scope command store.
/// The stored key is this prefix plus SHA-256(initiator ‖ Hub key) and the fingerprint
/// carries a `source` marker no customer body has: a customer `Idempotency-Key` can
/// never replay (or be replayed as) a Hub launch, and two users never share a key.
const HUB_KEY_PREFIX: &str = "cfmeet.communications:";
const HUB_SOURCE: &str = "cfmeet.communications";

/// The delegated user the Hub acts for. A body principal other than the signed
/// NATIVE_CF_USER initiating principal is refused: the Hub never acts for another user.
fn acting_user(
    d: &DelegatedPlatformContext,
    principal_id: Uuid,
) -> Result<crate::domain::PrincipalId, CallingError> {
    match d.initiating_principal() {
        Some(p)
            if p.principal_kind == DelegatedPrincipalKind::NativeCfUser
                && p.principal_id == principal_id
                && !principal_id.is_nil() =>
        {
            Ok(crate::domain::PrincipalId::from_uuid(principal_id))
        }
        _ => {
            tracing::warn!(
                "communications owner request for a principal other than the delegated user"
            );
            Err(CallingError::InternalPermissionDenied)
        }
    }
}

fn body<T: serde::de::DeserializeOwned>(
    body: Result<Json<T>, JsonRejection>,
) -> Result<T, CallingError> {
    crate::calls::app::http::parse_body(body)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchBody {
    idempotency_key: String,
    initiator_principal_id: Uuid,
    initiator_endpoint_id: Uuid,
    target_principal_ids: Vec<Uuid>,
    kind: crate::calls::kinds::CallKind,
}

/// The creation command key of a Hub launch (see [`HUB_KEY_PREFIX`]).
fn launch_key(
    initiator: Uuid,
    body: &LaunchBody,
) -> Result<crate::calls::service::CommandKey, CallingError> {
    use crate::calls::{
        kinds::CommandOperation,
        service::{fingerprint, CommandKey, IdempotencyKey},
    };
    use sha2::{Digest, Sha256};
    let key = body.idempotency_key.as_bytes();
    if key.is_empty()
        || key.len() > MAX_HUB_KEY_LEN
        || !key.iter().all(|b| (0x20..=0x7e).contains(b))
    {
        return Err(CallingError::Validation(
            "idempotencyKey must be 1-200 printable ASCII characters.",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(initiator.as_bytes());
    hasher.update([0u8]);
    hasher.update(key);
    let namespaced = format!("{HUB_KEY_PREFIX}{}", hex::encode(hasher.finalize()));
    let key = IdempotencyKey::parse(&namespaced).map_err(|_| CallingError::Internal)?;
    let canonical = json!({
        "source": HUB_SOURCE,
        "kind": body.kind,
        "initiatorPrincipalId": body.initiator_principal_id,
        "initiatorEndpointId": body.initiator_endpoint_id,
        "targetPrincipalIds": body.target_principal_ids,
    });
    Ok(CommandKey {
        operation: CommandOperation::CreateCall,
        key,
        fingerprint: fingerprint(CommandOperation::CreateCall, None, &canonical),
    })
}

/// The live platform authority a Hub launch is created under: the configured launch
/// credential of the delegated first-party service account, re-verified with Platform
/// Access exactly like a gateway-forwarded customer request (revocation, expiry and
/// suspension apply at once). Any failure fails closed (503); nothing is logged but the
/// bounded reason.
async fn launch_authority(
    state: &AppState,
    caller: &InternalCaller,
    d: &DelegatedPlatformContext,
) -> Result<crate::platform::context::PlatformAuthContext, CallingError> {
    use crate::platform::authentication::AuthenticationError;
    let Some(credential) = state.communications_launch_credential else {
        tracing::warn!("communications launch refused: no launch credential is configured");
        return Err(CallingError::ServiceUnavailable);
    };
    let request = crate::domain::RequestContext {
        request_id: caller.context().request_id().clone(),
        correlation_id: caller.context().correlation_id().clone(),
    };
    let ctx = match state
        .authenticator
        .authenticate_delegated(&d.clone().with_initiating_credential(credential), request)
        .await
    {
        Ok(ctx) => ctx,
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "communications launch refused: Platform Access unavailable");
            return Err(CallingError::ServiceUnavailable);
        }
        Err(AuthenticationError::Rejected { reason, .. })
        | Err(AuthenticationError::Throttled { reason, .. }) => {
            tracing::warn!(
                reason = reason.as_str(),
                "communications launch refused: launch credential rejected"
            );
            return Err(CallingError::ServiceUnavailable);
        }
    };
    if ctx.is_safety_only() || !ctx.has_scope(crate::platform::scopes::names::CALLS_WRITE) {
        tracing::warn!(
            safety_only = ctx.is_safety_only(),
            "communications launch refused: launch authority cannot create calls"
        );
        return Err(CallingError::ServiceUnavailable);
    }
    Ok(ctx)
}

/// CL-1: an app call by the delegated user, through `create_app_call` — the very domain
/// path of `POST /v1/calls` (personal policy before replay, initiator endpoint ACTIVE and
/// owned, principal categories, blocks, group limit, routing and ringing).
async fn launch(
    State(state): State<AppState>,
    caller: InternalCaller,
    request: Result<Json<LaunchBody>, JsonRejection>,
) -> Result<Json<Value>, CallingError> {
    use crate::{calls::app::CreateAppCall, domain::PrincipalId, endpoints::CallEndpointId};
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsLaunch,
    )?;
    let request = body(request)?;
    let initiator = acting_user(&d, request.initiator_principal_id)?;
    if request.target_principal_ids.is_empty()
        || request.target_principal_ids.len() > MAX_LAUNCH_TARGETS
    {
        return Err(CallingError::Validation(
            "targetPrincipalIds must hold at least one principal and stay within the group limit.",
        ));
    }
    let key = launch_key(initiator.as_uuid(), &request)?;
    let ctx = launch_authority(&state, &caller, &d).await?;
    let created = state
        .app
        .create_app_call(
            &ctx,
            CreateAppCall {
                kind: request.kind,
                initiator,
                initiator_endpoint: CallEndpointId::from_uuid(request.initiator_endpoint_id),
                targets: request
                    .target_principal_ids
                    .iter()
                    .map(|p| PrincipalId::from_uuid(*p))
                    .collect(),
                initiator_display_name: None,
                privacy_relay_session: None,
                key,
            },
        )
        .await?;
    let call = &created.body["call"];
    if !call["callId"].is_string() {
        return Err(CallingError::Internal);
    }
    Ok(Json(json!({
        "schemaVersion": 1,
        "callId": call["callId"],
        "state": call["state"],
        "kind": call["kind"],
        "version": call["version"],
        "createdAt": call["createdAt"],
        "replayed": created.replayed,
    })))
}

/// One Call of the delegated user's canonical history (CL-2 / CL-3).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CallEntry {
    call_id: String,
    kind: &'static str,
    state: &'static str,
    terminal: bool,
    version: i64,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    answered_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<i64>,
    direction: &'static str,
    outcome: &'static str,
    missed: bool,
    counterpart_principal_ids: Vec<Uuid>,
}

impl From<&crate::calls::app::history::HistoryEntry> for CallEntry {
    fn from(e: &crate::calls::app::history::HistoryEntry) -> Self {
        use crate::calls::app::history::{CallHistoryEntryView, HistoryDirection};
        // The customer history view is the single formatting of a history entry.
        let view = CallHistoryEntryView::from(e);
        Self {
            call_id: view.call.call_id,
            kind: view.call.kind,
            state: view.call.state,
            terminal: e.call.state.is_terminal(),
            version: view.call.version,
            created_at: view.call.created_at,
            answered_at: view.call.answered_at,
            ended_at: view.call.ended_at,
            duration_ms: view.duration_ms,
            direction: match e.participation.direction {
                HistoryDirection::Inbound => "INCOMING",
                HistoryDirection::Outbound => "OUTGOING",
            },
            outcome: view.participation.outcome,
            missed: view.participation.missed,
            counterpart_principal_ids: e.counterparts.iter().map(|c| c.principal_id).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ParticipationBody {
    principal_id: Uuid,
    call_ids: Vec<Uuid>,
}

/// CL-2: the delegated user's entries for the given Calls; Calls it does not take part
/// in (or of another scope) are silently omitted.
async fn participation(
    State(state): State<AppState>,
    caller: InternalCaller,
    request: Result<Json<ParticipationBody>, JsonRejection>,
) -> Result<Json<Value>, CallingError> {
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsRead,
    )?;
    let request = body(request)?;
    let principal = acting_user(&d, request.principal_id)?;
    let mut distinct = request.call_ids.clone();
    distinct.sort();
    distinct.dedup();
    if request.call_ids.is_empty()
        || request.call_ids.len() > MAX_PARTICIPATION_CALLS
        || distinct.len() != request.call_ids.len()
    {
        return Err(CallingError::Validation(
            "callIds must hold 1-50 distinct Calls.",
        ));
    }
    let entries = state
        .app
        .call_participation(CallScope::of_delegated(&d), principal, &request.call_ids)
        .await?;
    let calls: Vec<CallEntry> = entries.iter().map(CallEntry::from).collect();
    Ok(Json(json!({"schemaVersion": 1, "calls": calls})))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryBody {
    principal_id: Uuid,
    limit: u64,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    missed: Option<bool>,
}

/// CL-3: one page of the delegated user's canonical history (the `GET /v1/calls?
/// principalId` query: same derivation, order and keyset cursor).
async fn history(
    State(state): State<AppState>,
    caller: InternalCaller,
    request: Result<Json<HistoryBody>, JsonRejection>,
) -> Result<Json<Value>, CallingError> {
    use crate::calls::{
        app::history::HistoryFilter, read_model::CallCursor, service::ListCallsQuery,
    };
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsRead,
    )?;
    let request = body(request)?;
    let principal = acting_user(&d, request.principal_id)?;
    if request.limit == 0 || request.limit > ListCallsQuery::MAX_LIMIT {
        return Err(CallingError::Validation(
            "limit must be an integer between 1 and 100.",
        ));
    }
    let cursor = request
        .cursor
        .as_deref()
        .map(|c| CallCursor::decode(c).ok_or(CallingError::Validation("cursor is invalid.")))
        .transpose()?;
    let page = state
        .app
        .call_history(
            CallScope::of_delegated(&d),
            principal,
            &ListCallsQuery {
                limit: request.limit,
                cursor,
                ..Default::default()
            },
            HistoryFilter {
                missed: request.missed,
                ..Default::default()
            },
            None,
        )
        .await?;
    let data: Vec<CallEntry> = page.entries.iter().map(CallEntry::from).collect();
    Ok(Json(json!({
        "schemaVersion": 1,
        "data": data,
        "nextCursor": page.next_cursor.map(|c| c.encode()),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointBody {
    principal_id: Uuid,
    endpoint_id: Uuid,
}

/// The endpoint `id` of `principal` in `scope`, if it is that principal's (a foreign or
/// unknown endpoint is `None`, never distinguished).
async fn owned_endpoint(
    state: &AppState,
    scope: CallScope,
    principal: crate::domain::PrincipalId,
    id: crate::endpoints::CallEndpointId,
) -> Result<Option<crate::endpoints::CallEndpoint>, CallingError> {
    match state.endpoints.get(scope, id).await {
        Ok(endpoint) if endpoint.principal_id == principal => Ok(Some(endpoint)),
        Ok(_) | Err(CallError::NotFound) => Ok(None),
        Err(other) => Err(other.into()),
    }
}

/// CL-4: whether the delegated user owns the endpoint, and its status and kind.
async fn verify_endpoint(
    State(state): State<AppState>,
    caller: InternalCaller,
    request: Result<Json<EndpointBody>, JsonRejection>,
) -> Result<Json<Value>, CallingError> {
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsRead,
    )?;
    let request = body(request)?;
    let principal = acting_user(&d, request.principal_id)?;
    let endpoint = owned_endpoint(
        &state,
        CallScope::of_delegated(&d),
        principal,
        crate::endpoints::CallEndpointId::from_uuid(request.endpoint_id),
    )
    .await?;
    Ok(Json(match endpoint {
        Some(e) => json!({
            "schemaVersion": 1,
            "owned": true,
            "status": e.status.as_str(),
            "endpointKind": e.kind.as_str(),
        }),
        None => json!({"schemaVersion": 1, "owned": false, "status": null, "endpointKind": null}),
    }))
}

/// CL-5: the canonical endpoint revoke of `DELETE /v1/calling/endpoints/{id}` (idempotent;
/// a newly revoked endpoint's live ring offers end now). 404 unless the user owns it.
async fn revoke_endpoint(
    State(state): State<AppState>,
    caller: InternalCaller,
    request: Result<Json<EndpointBody>, JsonRejection>,
) -> Result<Json<Value>, CallingError> {
    let d = own(
        &state,
        &caller,
        InternalPermission::CallingCommunicationsApply,
    )?;
    let request = body(request)?;
    let principal = acting_user(&d, request.principal_id)?;
    let scope = CallScope::of_delegated(&d);
    let id = crate::endpoints::CallEndpointId::from_uuid(request.endpoint_id);
    // The owner never changes, so the ownership read cannot race the revoke.
    if owned_endpoint(&state, scope, principal, id)
        .await?
        .is_none()
    {
        return Err(CallingError::NotFound);
    }
    let (_, newly) = state.endpoints.revoke(scope, id).await?;
    if newly {
        state.app.cancel_endpoint_invitations(id).await?;
    }
    Ok(Json(json!({
        "schemaVersion": 1,
        "revoked": true,
        "alreadyRevoked": !newly,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(key: &str, initiator: Uuid) -> LaunchBody {
        LaunchBody {
            idempotency_key: key.to_string(),
            initiator_principal_id: initiator,
            initiator_endpoint_id: Uuid::new_v4(),
            target_principal_ids: vec![Uuid::new_v4()],
            kind: crate::calls::kinds::CallKind::Voice,
        }
    }

    #[test]
    fn hub_launch_keys_are_namespaced_per_initiator_and_bounded() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let body = launch("client-key 1", a);
        let ka = launch_key(a, &body).unwrap();
        assert!(ka.key.as_str().starts_with(HUB_KEY_PREFIX));
        assert_ne!(ka.key.as_str(), "client-key 1");
        assert_eq!(ka.key.as_str().len(), HUB_KEY_PREFIX.len() + 64);
        // Deterministic for replays, distinct per user.
        assert_eq!(launch_key(a, &body).unwrap().key, ka.key);
        assert_ne!(launch_key(b, &body).unwrap().key, ka.key);
        // The fingerprint covers the request, so a changed request under a key conflicts.
        let mut other = launch("client-key 1", a);
        other.initiator_endpoint_id = body.initiator_endpoint_id;
        other.target_principal_ids = body.target_principal_ids.clone();
        assert_eq!(launch_key(a, &other).unwrap().fingerprint, ka.fingerprint);
        other.kind = crate::calls::kinds::CallKind::Video;
        assert_ne!(launch_key(a, &other).unwrap().fingerprint, ka.fingerprint);
        for bad in ["", "\u{7f}", "é", &"k".repeat(201)] {
            assert!(launch_key(a, &launch(bad, a)).is_err(), "{bad:?}");
        }
        assert!(launch_key(a, &launch(&"k".repeat(200), a)).is_ok());
    }

    #[test]
    fn only_the_delegated_native_user_is_acted_for() {
        let me = Uuid::new_v4();
        let scope = DelegatedPlatformContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            ApplicationEnvironmentKind::Production,
            Uuid::new_v4(),
        );
        assert!(acting_user(&scope, me).is_err());
        let native = scope
            .clone()
            .with_initiating_principal(me, DelegatedPrincipalKind::NativeCfUser);
        assert_eq!(acting_user(&native, me).unwrap().as_uuid(), me);
        assert!(acting_user(&native, Uuid::new_v4()).is_err());
        let external = scope
            .with_initiating_principal(me, DelegatedPrincipalKind::ExternalApplicationPrincipal);
        assert!(acting_user(&external, me).is_err());
    }
}
