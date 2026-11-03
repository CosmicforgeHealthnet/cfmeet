use crate::{
    error::{Error, Result},
    State,
};
use axum::{
    extract::{Request, State as HttpState},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{
    DelegatedPlatformContext, InternalPermission, InternalServiceContext, OutboundServiceRequest,
    ServiceId,
};
use serde_json::{json, Value};

pub async fn middleware(
    HttpState(s): HttpState<State>,
    mut request: Request,
    next: Next,
) -> Response {
    let ctx = match s.config.verifier.verify_headers(request.headers()) {
        Ok(c) => c,
        Err(_) => return Error::Authentication.into_response(),
    };
    let permission = match request.uri().path() {
        "/internal/v1/communications/dispatch" => InternalPermission::CommunicationsUserForward,
        "/internal/v1/communications/policy/check"
        | "/internal/v1/communications/principals/resolve" => {
            InternalPermission::CommunicationsPolicyCheck
        }
        "/internal/v1/communications/account-events" => {
            InternalPermission::CommunicationsAccountApply
        }
        "/internal/v1/communications/block-events" => InternalPermission::CommunicationsBlockApply,
        "/internal/v1/communications/backfill"
        | "/internal/v1/communications/replay"
        | "/internal/v1/communications/metrics" => InternalPermission::CommunicationsBackfill,
        _ => return Error::NotFound.into_response(),
    };
    if ctx.require_permission(permission).is_err() {
        return Error::Forbidden.into_response();
    }
    // Receiver-owned replay protection survives restarts and multiple Hub instances.
    // A retry uses a new signed hop and the same durable command/event idempotency key.
    let inserted=crate::store::execute(&s.store.db,
        "INSERT INTO communication_service_nonces(issuer,token_id,expires_at) VALUES($1,$2,to_timestamp($3::bigint)+interval '60 seconds') ON CONFLICT DO NOTHING",
        vec![ctx.authenticated_service().as_str().into(),ctx.token_id().as_str().into(),ctx.expires_at().into()]).await;
    match inserted {
        Ok(1) => {}
        Ok(_) => return Error::Authentication.into_response(),
        Err(e) => return e.into_response(),
    }
    let issuer = ctx.authenticated_service().as_str().to_owned();
    let route = request.uri().path().to_owned();
    let started = std::time::Instant::now();
    request.extensions_mut().insert(ctx);
    let response = next.run(request).await;
    tracing::info!(
        service = "communications",
        caller = issuer,
        route,
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "signed request completed"
    );
    response
}
pub fn delegated(ctx: &InternalServiceContext) -> Result<DelegatedPlatformContext> {
    ctx.delegated_platform_context()
        .cloned()
        .ok_or(Error::Forbidden)
}

/// Validate the signed authority chain with its owner before querying scoped data.
pub async fn validate_scope(s: &State, scope: &DelegatedPlatformContext) -> Result<()> {
    let req = OutboundServiceRequest::new_operation(ServiceId::platform_access())
        .with_delegated_context(scope.clone());
    let response = s
        .config
        .client
        .get(&req, "/internal/v1/identity/context")
        .await
        .map_err(|_| Error::Unavailable)?;
    if response.status() != 200 {
        return Err(if response.status() < 500 {
            Error::Forbidden
        } else {
            Error::Unavailable
        });
    }
    let value: Value = response.json().map_err(|_| Error::Unavailable)?;
    // A successful canonical context read is the authority; ids are never taken from
    // consumer request JSON. Only first-party native accounts can create profiles.
    if value["authorityMode"] != "FULL" || value["tenant"]["classification"] != "FIRST_PARTY" {
        return Err(Error::Forbidden);
    }
    // Non-secret authority reference for signed outbox delivery after process restart.
    crate::store::execute(&s.store.db,
        "INSERT INTO communication_scope_authorities(tenant_id,application_id,environment_id,service_account_id,environment_kind) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,application_id,environment_id) DO UPDATE SET service_account_id=$4,environment_kind=$5",
        vec![scope.tenant_id().into(),scope.application_id().into(),scope.application_environment_id().into(),scope.initiating_service_account_id().into(),serde_json::to_value(scope.application_environment_kind()).map_err(|_|Error::Unavailable)?.as_str().ok_or(Error::Unavailable)?.into()]).await?;
    Ok(())
}
pub async fn account_status(
    s: &State,
    scope: &DelegatedPlatformContext,
    account_ref: &str,
) -> Result<Value> {
    let req = OutboundServiceRequest::new_operation(ServiceId::meeting_service())
        .with_delegated_context(scope.clone());
    let r = s
        .config
        .client
        .post_json(
            &req,
            "/internal/v1/accounts/communications/status",
            &json!({"accountReference":account_ref}),
        )
        .await
        .map_err(|_| Error::Unavailable)?;
    if r.status() != 200 {
        return Err(Error::Unavailable);
    }
    r.json().map_err(|_| Error::Unavailable)
}
pub async fn principals_active(
    s: &State,
    scope: &DelegatedPlatformContext,
    profiles: &[&crate::model::Profile],
) -> Result<bool> {
    let request = OutboundServiceRequest::new_operation(ServiceId::platform_access())
        .with_delegated_context(scope.clone());
    let profiles: std::collections::BTreeMap<_, _> =
        profiles.iter().map(|p| (p.principal_id, *p)).collect();
    let references: Vec<_> = profiles
        .values()
        .map(|p| json!({"principalId":p.principal_id}))
        .collect();
    let response = s
        .config
        .client
        .post_json(
            &request,
            "/internal/v1/principals/resolve",
            &json!({"references":references}),
        )
        .await
        .map_err(|_| Error::Unavailable)?;
    if response.status() != 200 {
        return Err(Error::Unavailable);
    }
    let result: Value = response.json().map_err(|_| Error::Unavailable)?;
    let results = result["results"]
        .as_array()
        .filter(|r| r.len() == profiles.len())
        .ok_or(Error::Unavailable)?;
    Ok(profiles.values().zip(results).all(|(p, r)| {
        r["outcome"] == "RESOLVED"
            && r["principal"]["id"] == p.principal_id.to_string()
            && r["principal"]["kind"] == "NATIVE_CF_USER"
            && r["principal"]["status"] == "ACTIVE"
    }))
}
pub async fn verify_principal(
    s: &State,
    scope: &DelegatedPlatformContext,
    account: &crate::model::VerifiedAccount,
) -> Result<()> {
    let init = scope.initiating_principal().ok_or(Error::Forbidden)?;
    if init.principal_id != account.principal_id
        || init.principal_kind != cf_service_auth::DelegatedPrincipalKind::NativeCfUser
    {
        return Err(Error::Forbidden);
    }
    let req = OutboundServiceRequest::new_operation(ServiceId::platform_access())
        .with_delegated_context(scope.clone());
    let response = s
        .config
        .client
        .post_json(
            &req,
            "/internal/v1/principals/resolve",
            &json!({"references":[{"principalId":account.principal_id}]}),
        )
        .await
        .map_err(|_| Error::Unavailable)?;
    if response.status() != 200 {
        return Err(Error::Forbidden);
    }
    let value: Value = response.json().map_err(|_| Error::Unavailable)?;
    // Exact principal wire is checked by the adapter and covered by real-service tests.
    let principal = value["results"]
        .as_array()
        .and_then(|v| v.first())
        .ok_or(Error::Forbidden)?;
    if principal["outcome"] != "RESOLVED" {
        return Err(Error::Forbidden);
    }
    let p = principal.get("principal").unwrap_or(principal);
    if p["id"] != account.principal_id.to_string()
        || p["kind"] != "NATIVE_CF_USER"
        || p["status"] != "ACTIVE"
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}
