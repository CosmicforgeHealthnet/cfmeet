use crate::{
    auth,
    error::{Error, Result},
    model::*,
    store::{self, execute},
    State,
};
use axum::{
    extract::{DefaultBodyLimit, State as HttpState},
    http::{HeaderValue, StatusCode},
    middleware::{from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use cf_service_auth::InternalServiceContext;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use uuid::Uuid;

pub fn app(state: State) -> Router {
    let internal = Router::new()
        .route("/internal/v1/communications/dispatch", post(dispatch))
        .route(
            "/internal/v1/communications/policy/check",
            post(policy_check),
        )
        .route(
            "/internal/v1/communications/principals/resolve",
            post(resolve_principal),
        )
        .route(
            "/internal/v1/communications/account-events",
            post(crate::events::account_event),
        )
        .route(
            "/internal/v1/communications/block-events",
            post(crate::events::block_event),
        )
        .route(
            "/internal/v1/communications/backfill",
            post(crate::events::backfill),
        )
        .route(
            "/internal/v1/communications/replay",
            post(crate::events::replay),
        )
        .route("/internal/v1/communications/metrics", get(metrics))
        .fallback(|| async { Error::NotFound })
        .layer(from_fn_with_state(state.clone(), auth::middleware));
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"status":"ok","service":"communications"})) }),
        )
        .route("/ready", get(ready))
        .merge(internal)
        .layer(map_response(normalize_rejection))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(state)
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolvePrincipal {
    actor_principal_id: Uuid,
    cf_meet_id: String,
    #[serde(default)]
    purpose: Option<cf_communications_policy::Action>,
}
/// A private translation for authoritative owners; consumer discovery never exposes it.
async fn resolve_principal(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<ResolvePrincipal>,
) -> Result<Json<Value>> {
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let target = s
        .store
        .by_public_id(
            &s.store.db,
            cf_communications_policy::Scope::of(&scope),
            &input.cf_meet_id,
        )
        .await?;
    use cf_communications_policy::Action;
    let action = match (ctx.authenticated_service(), input.purpose) {
        (caller, None | Some(Action::DirectCall))
            if caller == &cf_service_auth::ServiceId::calling() =>
        {
            Action::DirectCall
        }
        (caller, None | Some(Action::DirectConversation))
            if caller == &cf_service_auth::ServiceId::messaging() =>
        {
            Action::DirectConversation
        }
        (caller, Some(Action::GroupInvitation))
            if caller == &cf_service_auth::ServiceId::messaging() =>
        {
            Action::GroupInvitation
        }
        _ => return Err(Error::Forbidden),
    };
    let decision = s
        .check_policy(
            &scope,
            &cf_communications_policy::PolicyCheck {
                action,
                actor_principal_id: input.actor_principal_id,
                target_principal_id: target.principal_id,
                credential_issued_at: None,
                credential_principal_id: None,
            },
        )
        .await?;
    if !decision.allowed {
        return Err(Error::NotFound);
    }
    Ok(Json(
        json!({"schemaVersion":1,"principalId":target.principal_id,"kind":"NATIVE_CF_USER","policyVersion":decision.policy_version}),
    ))
}
async fn normalize_rejection(response: Response) -> Response {
    if !response.status().is_success()
        && !response
            .headers()
            .get("content-type")
            .is_some_and(|h| h.as_bytes().starts_with(b"application/json"))
    {
        return match response.status().as_u16() {
            401 => Error::Authentication,
            403 => Error::Forbidden,
            404 => Error::NotFound,
            500..=599 => Error::Unavailable,
            _ => Error::Validation,
        }
        .into_response();
    }
    response
}
async fn ready(HttpState(s): HttpState<State>) -> impl IntoResponse {
    let ready = communications_migration::ready(&s.store.db)
        .await
        .unwrap_or(false);
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(
            json!({"status":if ready {"ready"} else {"not_ready"},"service":"communications","databaseMigrationsCurrent":ready}),
        ),
    )
}
async fn metrics(HttpState(s): HttpState<State>) -> Json<Value> {
    Json(s.metrics.snapshot())
}
async fn policy_check(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(check): Json<cf_communications_policy::PolicyCheck>,
) -> Result<Json<cf_communications_policy::Decision>> {
    if matches!(
        check.action,
        cf_communications_policy::Action::MessageNotification
            | cf_communications_policy::Action::GroupNotification
    ) && ctx.authenticated_service() != &cf_service_auth::ServiceId::messaging()
    {
        return Err(Error::Forbidden);
    }
    s.metrics.requests.fetch_add(1, Ordering::Relaxed);
    let result = s.check_policy(&auth::delegated(&ctx)?, &check).await?;
    if !result.allowed {
        s.metrics.denied.fetch_add(1, Ordering::Relaxed);
    }
    Ok(Json(result))
}
async fn dispatch(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(request): Json<Dispatch>,
) -> Result<Response> {
    s.metrics.requests.fetch_add(1, Ordering::Relaxed);
    let scope = auth::delegated(&ctx)?;
    let profile = s.ensure_profile(&scope, &request.account).await?;
    let result = dispatch_inner(&s, &scope, &profile, request).await;
    match result {
        Ok(value) => {
            let mut r = Json(value).into_response();
            r.headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            Ok(r)
        }
        Err(e) => {
            s.metrics.denied.fetch_add(1, Ordering::Relaxed);
            Err(e)
        }
    }
}
fn body(request: &Dispatch) -> Value {
    request.body.clone().unwrap_or_else(|| json!({}))
}
fn key(request: &Dispatch) -> Result<Uuid> {
    request.idempotency_key.ok_or(Error::Validation)
}
fn id(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| Error::NotFound)
}
fn page(request: &Dispatch) -> Result<(i64, Uuid)> {
    if request
        .query
        .keys()
        .any(|k| !matches!(k.as_str(), "limit" | "cursor"))
    {
        return Err(Error::Validation);
    }
    let limit = request
        .query
        .get("limit")
        .map(|v| v.parse::<i64>())
        .transpose()
        .map_err(|_| Error::Validation)?
        .unwrap_or(30);
    if !(1..=100).contains(&limit) {
        return Err(Error::Validation);
    }
    let cursor = request
        .query
        .get("cursor")
        .map(|v| Uuid::parse_str(v))
        .transpose()
        .map_err(|_| Error::Validation)?
        .unwrap_or(Uuid::nil());
    Ok((limit, cursor))
}
async fn dispatch_inner(
    s: &State,
    scope: &cf_service_auth::DelegatedPlatformContext,
    p: &Profile,
    r: Dispatch,
) -> Result<Value> {
    if r.path.len() > 180
        || r.path.contains(['%', '?', '#', '\\'])
        || r.path.split('/').any(|s| s == ".." || s == ".")
    {
        return Err(Error::Validation);
    }
    let parts: Vec<&str> = r.path.trim_matches('/').split('/').collect();
    let method = r.method.as_str();
    match (method, parts.as_slice()) {
        ("GET", ["me"]) => s.me(p).await,
        ("GET", ["activation"]) => Ok(
            json!({"schemaVersion":1,"state":p.activation_state,"version":p.version,"phoneVerificationRequired":false}),
        ),
        ("GET", ["me", "capabilities"]) => Ok(s.capabilities(scope, p).await),
        ("GET", ["me", "public-id"]) => Ok(
            json!({"schemaVersion":1,"cfMeetId":s.store.current_public_id(&s.store.db,p.id).await?,"version":p.version}),
        ),
        ("POST", ["me", "public-id", "rotate"]) => {
            s.rotate_public_id(p, &r.account, body(&r), key(&r)?).await
        }
        ("GET", ["me", "privacy"]) => s.get_privacy(p).await,
        ("PUT", ["me", "privacy"]) => s.set_privacy(p, body(&r), key(&r)?).await,
        ("GET", ["me", "qr"]) => {
            s.create_invite(
                p,
                json!({"purpose":"CONNECT","expiresInSeconds":600,"maxUses":1}),
                key(&r)?,
                true,
            )
            .await
        }
        ("POST", ["invites"]) => s.create_invite(p, body(&r), key(&r)?, false).await,
        ("GET", ["invites"]) => {
            let (n, c) = page(&r)?;
            s.list_invites(p, n, c).await
        }
        ("DELETE", ["invites", invite]) => s.revoke_invite(p, id(invite)?, key(&r)?).await,
        ("POST", ["discovery", "invites", "resolve"]) => {
            s.resolve_invite(scope, p, body(&r), key(&r)?).await
        }
        ("POST", ["discovery", "public-id", "resolve"]) => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Resolve {
                cf_meet_id: String,
            }
            let resolve: Resolve =
                serde_json::from_value(body(&r)).map_err(|_| Error::Validation)?;
            s.limit_attempt(p, "discovery.resolve", 120).await?;
            let target = s
                .store
                .by_public_id(&s.store.db, p.scope(), &resolve.cf_meet_id)
                .await?;
            s.visible_profile(scope, p, &target, false).await
        }
        ("GET", ["profiles", public_id]) => {
            s.limit_attempt(p, "discovery.resolve", 120).await?;
            let target = s
                .store
                .by_public_id(&s.store.db, p.scope(), public_id)
                .await?;
            s.visible_profile(scope, p, &target, false).await
        }
        ("POST", ["profiles", public_id, "reports"]) => {
            report(s, p, public_id, body(&r), key(&r)?).await
        }
        ("PUT", ["profiles", public_id, "block"])
        | ("DELETE", ["profiles", public_id, "block"]) => {
            let target = s
                .store
                .by_public_id(&s.store.db, p.scope(), public_id)
                .await?;
            s.set_block(p, &target, method == "PUT", key(&r)?).await
        }
        ("GET", ["blocks"]) => {
            let (limit, cursor) = page(&r)?;
            let values:Vec<Value>=store::rows(&s.store.db,"SELECT jsonb_build_object('cfMeetId',i.public_id,'profileId',b.blocked_profile_id) doc FROM communication_block_bridge b JOIN communication_public_ids i ON i.profile_id=b.blocked_profile_id AND i.current WHERE b.blocker_profile_id=$1 AND b.source='COMMUNICATIONS' AND b.active AND b.blocked_profile_id>$2 ORDER BY b.blocked_profile_id LIMIT $3",vec![p.id.into(),cursor.into(),(limit+1).into()]).await?;
            let more = values.len() > limit as usize;
            let mut data = Vec::new();
            let mut last = None;
            for v in values.into_iter().take(limit as usize) {
                last = v["profileId"].as_str().map(str::to_owned);
                data.push(json!({"cfMeetId":v["cfMeetId"],"blocked":true}));
            }
            Ok(json!({"schemaVersion":1,"data":data,"nextCursor":if more {last} else {None}}))
        }
        ("GET", ["contacts"]) => {
            let (n, c) = page(&r)?;
            s.list_contacts(p, n, c).await
        }
        ("GET", ["contacts", contact]) => s.contact_view(p, id(contact)?).await,
        ("PATCH", ["contacts", contact]) => {
            s.patch_contact(p, id(contact)?, body(&r), key(&r)?).await
        }
        ("DELETE", ["contacts", contact]) => s.remove_contact(p, id(contact)?, key(&r)?).await,
        ("GET", ["contacts", contact, "common-groups"]) => {
            s.common_groups(scope, p, id(contact)?).await
        }
        ("POST", ["contact-requests"]) => s.send_request(scope, p, body(&r), key(&r)?).await,
        ("GET", ["contact-requests", direction @ ("incoming" | "outgoing")]) => {
            let (n, c) = page(&r)?;
            s.list_requests(p, *direction == "incoming", n, c).await
        }
        ("POST", ["contact-requests", request, action @ ("accept" | "decline")]) => {
            s.decide_request(scope, p, id(request)?, action, body(&r), key(&r)?)
                .await
        }
        ("DELETE", ["contact-requests", request]) => {
            s.decide_request(scope, p, id(request)?, "cancel", body(&r), key(&r)?)
                .await
        }
        _ => Err(Error::NotFound),
    }
}
async fn report(s: &State, p: &Profile, cf_id: &str, body: Value, key: Uuid) -> Result<Value> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Report {
        reason: String,
    }
    let request: Report = serde_json::from_value(body.clone()).map_err(|_| Error::Validation)?;
    if !matches!(
        request.reason.as_str(),
        "SPAM" | "HARASSMENT" | "IMPERSONATION" | "OTHER"
    ) {
        return Err(Error::Validation);
    }
    s.limit_attempt(p, "profile.report", 5).await?;
    let target = s.store.by_public_id(&s.store.db, p.scope(), cf_id).await?;
    if p.id == target.id {
        return Err(Error::Validation);
    }
    let txn = s.store.begin().await?;
    let bound = json!({"cfMeetId":cf_id,"body":body});
    if let Some(v) = s
        .store
        .idempotency(&txn, p, "profile.report", key, &bound)
        .await?
    {
        txn.commit().await?;
        return Ok(v);
    }
    let id = Uuid::new_v4();
    execute(&txn,"INSERT INTO communication_profile_reports(id,reporter_profile_id,target_profile_id,reason) VALUES($1,$2,$3,$4)",vec![id.into(),p.id.into(),target.id.into(),request.reason.clone().into()]).await?;
    s.store
        .audit(
            &txn,
            p,
            "PROFILE_REPORTED",
            Some(id),
            json!({"category":request.reason}),
        )
        .await?;
    let result = json!({"schemaVersion":1,"reportId":id,"received":true});
    s.store
        .finish_idempotency(&txn, p, "profile.report", key, result.clone())
        .await?;
    txn.commit().await?;
    Ok(result)
}
