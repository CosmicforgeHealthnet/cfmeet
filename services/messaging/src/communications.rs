//! Personal relationship policy. Messaging remains the conversation and security owner.
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
    let security = devices > 0
        && s.tree_heads.is_some()
        && s.witnesses.quorum > 0
        && signatures.len() >= s.witnesses.quorum;
    let available = security && policy.sending_enabled;
    Ok(Json(
        json!({"available":available,"securityReady":security,"reason":if !security {"DEVICE_SECURITY_SETUP_REQUIRED"} else if !available {"DISABLED"} else {"AVAILABLE"}}),
    ))
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
    }
    txn.commit().await.map_err(Error::from)?;
    Ok(Json(json!({"applied":n==1,"duplicate":n==0})))
}
