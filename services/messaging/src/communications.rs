//! Personal relationship policy. Messaging remains the conversation and security owner.
mod owner;
use crate::{
    conversations::service::ConversationRow,
    error::{Error, ErrorCode, Result},
    http::{
        error::ApiError,
        middleware::{delegated_context, InternalCaller},
    },
    identity::TrustedContext,
    state::AppState,
};
use axum::{extract::State, routing::post, Json, Router};
use cf_communications_policy::{Action, PolicyCheck, PolicyClient, PolicyError, Scope};
use cf_service_auth::InternalPermission;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub fn scope(ctx: &TrustedContext) -> Scope {
    Scope {
        tenant_id: ctx.scope.tenant_id,
        application_id: ctx.scope.application_id,
        environment_id: ctx.scope.environment_id,
    }
}
/// Normalize a consumer CF Meet ID into the existing canonical request model.
/// The actor and authority chain are authenticated, never accepted from this body.
pub async fn resolve_address(
    state: &AppState,
    ctx: &TrustedContext,
    actor: Option<Uuid>,
    body: &mut Value,
    public_field: &str,
    principal_field: &str,
    purpose: Action,
) -> Result<()> {
    let Some(value) = body.get(public_field) else {
        return Ok(());
    };
    if body.get(principal_field).is_some() {
        return Err(Error::validation(
            "provide exactly one recipient identifier",
        ));
    }
    let id = value
        .as_str()
        .filter(|id| {
            id.len() == 29
                && id.starts_with("CF-")
                && id.as_bytes()[3..]
                    .iter()
                    .all(|c| c.is_ascii_uppercase() || (b'2'..=b'7').contains(c))
        })
        .ok_or_else(|| Error::validation("invalid CF Meet ID"))?;
    let client = state
        .communications
        .as_ref()
        .filter(|p| p.governs(scope(ctx)))
        .ok_or_else(|| Error::code(ErrorCode::DeliveryRefused))?;
    let actor = actor.ok_or_else(|| Error::code(ErrorCode::ActingPrincipalRequired))?;
    let principal = client
        .resolve_principal(ctx.scope.delegated(), actor, id, purpose)
        .await
        .map_err(|e| match e {
            PolicyError::Denied => Error::code(ErrorCode::DeliveryRefused),
            PolicyError::Unavailable => Error::reason(
                ErrorCode::ServiceUnavailable,
                "COMMUNICATIONS_POLICY_UNAVAILABLE",
            ),
        })?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| Error::validation("object required"))?;
    object.remove(public_field);
    object.insert(principal_field.to_owned(), json!(principal));
    Ok(())
}
impl AppState {
    pub fn with_communications(mut self, client: Option<PolicyClient>) -> Self {
        self.communications = client;
        self
    }
}
pub async fn require(
    state: &AppState,
    ctx: &TrustedContext,
    action: Action,
    actor: Uuid,
    target: Uuid,
) -> Result<()> {
    let Some(client) = &state.communications else {
        return Ok(());
    };
    if !client.governs(scope(ctx)) {
        return Ok(());
    }
    client
        .check(
            ctx.scope.delegated(),
            &PolicyCheck {
                action,
                actor_principal_id: actor,
                target_principal_id: target,
                credential_issued_at: ctx.personal_session_issued_at,
                credential_principal_id: ctx.personal_session_issued_at.map(|_| actor),
                conversation_id: None,
            },
        )
        .await
        .map_err(|e| match e {
            PolicyError::Denied => Error::code(ErrorCode::DeliveryRefused),
            PolicyError::Unavailable => Error::reason(
                ErrorCode::ServiceUnavailable,
                "COMMUNICATIONS_POLICY_UNAVAILABLE",
            ),
        })
}
#[derive(FromQueryResult)]
struct PrincipalRow {
    principal_id: Uuid,
}
pub async fn require_send<C: ConnectionTrait>(
    db: &C,
    state: &AppState,
    ctx: &TrustedContext,
    c: &ConversationRow,
    actor: Uuid,
) -> Result<()> {
    let Some(client) = state.communications.as_ref() else {
        return Ok(());
    };
    if !client.governs(scope(ctx)) {
        return Ok(());
    }
    if c.context_class == "GROUP" {
        // Membership remains Messaging-owned. A suspended personal account cannot
        // bypass its current activation state through a group message or edit.
        return require(state, ctx, Action::PersonalActivity, actor, actor).await;
    }
    if c.context_class != "DIRECT" {
        return Ok(());
    }
    let targets=crate::db::all::<PrincipalRow,_>(db,"SELECT principal_id FROM messaging_conversation_members WHERE conversation_id=$1 AND state IN ('ACTIVE','INVITED') AND principal_id<>$2",vec![c.id.into(),actor.into()]).await?;
    if targets.len() != 1 {
        return Err(Error::code(ErrorCode::DeliveryRefused));
    }
    require(
        state,
        ctx,
        Action::MessageSend,
        actor,
        targets[0].principal_id,
    )
    .await
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/communications/blocks/check", post(blocks))
        .route("/communications/capabilities", post(capabilities))
        .route("/communications/common-groups", post(common_groups))
        .route("/communications/events", post(event))
        // Sections 9–23 owner interfaces (MS-1 … MS-7): read-only views and the
        // canonical device / push mutations, for the delegated user only.
        .route(
            "/communications/conversations/access",
            post(owner::conversation_access),
        )
        .route(
            "/communications/conversations/attachments",
            post(owner::conversation_attachments),
        )
        .route(
            "/communications/storage-summary",
            post(owner::storage_summary),
        )
        .route("/communications/devices/verify", post(owner::device_verify))
        .route("/communications/devices/revoke", post(owner::device_revoke))
        .route("/communications/devices/push", post(owner::device_push))
        .route("/communications/push-status", post(owner::push_status))
}
async fn own(
    state: &AppState,
    caller: &InternalCaller,
    permission: InternalPermission,
) -> std::result::Result<TrustedContext, ApiError> {
    caller.require_permission(permission)?;
    let ctx = delegated_context(state, caller).await?;
    if !ctx.first_party
        || !state
            .communications
            .as_ref()
            .is_some_and(|p| p.governs(scope(&ctx)))
    {
        return Err(ApiError::code(ErrorCode::NotFound));
    }
    Ok(ctx)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pair {
    actor_principal_id: Uuid,
    target_principal_id: Uuid,
}
async fn blocks(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<Pair>,
) -> std::result::Result<Json<Value>, ApiError> {
    let ctx = own(&s, &caller, InternalPermission::MessagingCommunicationsRead).await?;
    Ok(Json(
        json!({"blocked":crate::safety::blocks::blocked_between(&s.db,ctx.scope.environment_id,p.actor_principal_id,p.target_principal_id).await?}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Principal {
    principal_id: Uuid,
}
async fn capabilities(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<Principal>,
) -> std::result::Result<Json<Value>, ApiError> {
    let ctx = own(&s, &caller, InternalPermission::MessagingCommunicationsRead).await?;
    let devices=crate::db::scalar_i64(&s.db,"SELECT count(*) FROM messaging_devices d WHERE d.tenant_id=$1 AND d.application_id=$2 AND d.environment_id=$3 AND d.principal_id=$4 AND d.status='ACTIVE' AND EXISTS(SELECT 1 FROM messaging_key_packages k WHERE k.device_id=d.id AND k.status='AVAILABLE' AND k.not_before<=now() AND k.not_after>now())",vec![ctx.scope.tenant_id.into(),ctx.scope.application_id.into(),ctx.scope.environment_id.into(),p.principal_id.into()]).await?;
    let policy = crate::conversations::policy::current(&s.db, ctx.scope.environment_id).await?;
    let head = crate::transparency::tree_head(&s.db).await?;
    let signatures = crate::transparency::witness::cosignatures(&s.db, &s.witnesses, &head).await?;
    let security = s.devices.enabled
        && devices > 0
        && s.tree_heads.is_some()
        && s.witnesses.quorum > 0
        && signatures.len() >= s.witnesses.quorum
        && crate::operations::verify_request_checkpoint(&s)
            .await
            .is_ok()
        && !crate::operations::engaged(
            &s.db,
            crate::operations::KillSwitch::E2eeOnboarding,
            Some(ctx.scope.environment_id),
        )
        .await?
        && !crate::operations::engaged(
            &s.db,
            crate::operations::KillSwitch::KtProofs,
            Some(ctx.scope.environment_id),
        )
        .await?;
    let available = security && policy.sending_enabled;
    let managed = managed_capability(&policy, &s.realtime);
    // `available` / `securityReady` / `reason` keep their PRIVATE_E2EE meaning for older
    // Hub images; `managed` (Blueprint Section 14) reports MANAGED messaging separately.
    Ok(Json(
        json!({"available":available,"securityReady":security,"reason":if !security {"DEVICE_SECURITY_SETUP_REQUIRED"} else if !available {"DISABLED"} else {"AVAILABLE"},
            "managed":managed}),
    ))
}
/// MANAGED (server-readable) personal messaging: allowed when sending is enabled and a
/// personal context (DIRECT or GROUP) permits MANAGED. Live delivery without an E2EE
/// device is the session device of ADR 0049 amendment A1, which needs
/// `MESSAGING_SESSION_DEVICES_ENABLED` and the client-token signing key; otherwise MANAGED
/// still works and clients poll. Never implies any E2EE readiness.
fn managed_capability(
    policy: &crate::conversations::policy::EffectivePolicy,
    realtime: &crate::realtime::Realtime,
) -> Value {
    use crate::{conversations::ContextClass, domain::SecurityProfile};
    let allowed = [ContextClass::Direct, ContextClass::Group].iter().any(|c| {
        policy.contexts.get(c).is_some_and(|p| {
            p.enabled && p.allowed_profiles.contains(&SecurityProfile::Managed)
        })
    });
    let available = policy.sending_enabled && allowed;
    let realtime =
        available && realtime.settings.session_devices_enabled && realtime.keys.is_some();
    json!({"available":available,
        "reason":if available {"AVAILABLE"} else if !policy.sending_enabled {"DISABLED"} else {"MANAGED_NOT_ALLOWED"},
        "realtime":if realtime {"SESSION_DEVICE"} else {"POLLING"}})
}
async fn common_groups(
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(p): Json<Pair>,
) -> std::result::Result<Json<Value>, ApiError> {
    let ctx = own(&s, &caller, InternalPermission::MessagingCommunicationsRead).await?;
    #[derive(FromQueryResult)]
    struct Group {
        id: Uuid,
        context_class: String,
    }
    let groups=crate::db::all::<Group,_>(&s.db,"SELECT c.id,c.context_class FROM messaging_conversations c JOIN messaging_conversation_members a ON a.conversation_id=c.id AND a.principal_id=$4 AND a.state='ACTIVE' JOIN messaging_conversation_members b ON b.conversation_id=c.id AND b.principal_id=$5 AND b.state='ACTIVE' WHERE c.tenant_id=$1 AND c.application_id=$2 AND c.environment_id=$3 AND c.context_class='GROUP' AND c.lifecycle_state IN ('ACTIVE','LOCKED','CLOSED','ARCHIVED') ORDER BY c.id LIMIT 100",vec![ctx.scope.tenant_id.into(),ctx.scope.application_id.into(),ctx.scope.environment_id.into(),p.actor_principal_id.into(),p.target_principal_id.into()]).await?;
    Ok(Json(
        json!({"groups":groups.into_iter().map(|g|json!({"conversationId":g.id,"contextClass":g.context_class})).collect::<Vec<_>>() }),
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
    State(s): State<AppState>,
    caller: InternalCaller,
    Json(e): Json<Event>,
) -> std::result::Result<Json<Value>, ApiError> {
    let ctx = own(
        &s,
        &caller,
        InternalPermission::MessagingCommunicationsApply,
    )
    .await?;
    if e.schema_version != 1
        || e.version < 1
        || !e.kind.starts_with("communications.")
        || e.kind.len() > 80
    {
        return Err(Error::validation("Invalid Communications event"));
    }
    let digest = {
        use sha2::{Digest, Sha256};
        Sha256::digest(serde_json::to_vec(&json!({"aggregateId":e.aggregate_id,"version":e.version,"type":e.kind,"data":e.data})).map_err(|_|Error::code(ErrorCode::InternalError))?).to_vec()
    };
    let txn = s.db.begin().await.map_err(Error::from)?;
    let d = ctx.scope;
    let n=txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO messaging_communication_inbox(event_id,tenant_id,application_id,environment_id,request_digest) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",[e.event_id.into(),d.tenant_id.into(),d.application_id.into(),d.environment_id.into(),digest.clone().into()])).await.map_err(Error::from)?.rows_affected();
    if n == 0 {
        let r=txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT request_digest FROM messaging_communication_inbox WHERE event_id=$1 AND tenant_id=$2 AND application_id=$3 AND environment_id=$4",[e.event_id.into(),d.tenant_id.into(),d.application_id.into(),d.environment_id.into()])).await.map_err(Error::from)?.ok_or(Error::code(ErrorCode::NotFound))?;
        if r.try_get::<Vec<u8>>("", "request_digest")
            .map_err(Error::from)?
            != digest
        {
            return Err(Error::validation("Event identity conflict"));
        }
    }
    if let (Some(principal), Some(cutoff)) = (
        e.data["principalId"]
            .as_str()
            .and_then(|x| Uuid::parse_str(x).ok()),
        e.data["credentialsNotBefore"].as_i64(),
    ) {
        txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO messaging_communication_revocations(tenant_id,application_id,environment_id,principal_id,credentials_not_before) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,application_id,environment_id,principal_id) DO UPDATE SET credentials_not_before=GREATEST(messaging_communication_revocations.credentials_not_before,$5),updated_at=now()",[d.tenant_id.into(),d.application_id.into(),d.environment_id.into(),principal.into(),cutoff.into()])).await.map_err(Error::from)?;
        // Revoke sessions; preserve the canonical device identity, MLS state and history.
        txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE messaging_client_sessions SET status='REVOKED',revoked_at=now() WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4 AND status='ACTIVE' AND created_at<=to_timestamp($5::double precision)",[d.tenant_id.into(),d.application_id.into(),d.environment_id.into(),principal.into(),(cutoff as f64).into()])).await.map_err(Error::from)?;
        txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE messaging_push_registrations SET enabled=FALSE,disabled_reason='ACCOUNT_SESSION_REVOKED',updated_at=now() WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND principal_id=$4 AND updated_at<=to_timestamp($5::double precision)",[d.tenant_id.into(),d.application_id.into(),d.environment_id.into(),principal.into(),(cutoff as f64).into()])).await.map_err(Error::from)?;
    }
    txn.commit().await.map_err(Error::from)?;
    Ok(Json(json!({"applied":n==1,"duplicate":n==0})))
}
