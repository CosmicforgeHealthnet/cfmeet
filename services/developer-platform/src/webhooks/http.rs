//! Webhook routes.
//!
//! Customer API (machine credentials through the API Gateway; exactly Calling's contract
//! so the gateway can re-point it here at the cut-over):
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /v1/webhooks`, `GET /v1/webhooks` | `webhooks.manage` (+ every event type's read scope) / `webhooks.read` |
//! | `GET / PATCH / DELETE /v1/webhooks/{webhook_id}` | `webhooks.read` / `webhooks.manage` (+ read scopes) / `webhooks.manage` |
//! | `POST /v1/webhooks/{webhook_id}/rotate-secret` | `webhooks.manage` |
//! | `GET /v1/webhooks/{webhook_id}/deliveries` | `webhooks.read` |
//! | `POST /v1/webhook-deliveries/{delivery_id}/replay` | `webhooks.manage` |
//!
//! Developer Console (human workspace members; OWNER / ADMIN / DEVELOPER manage, ANALYST
//! reads; organization lifecycle rules) under `/v1/environments/{environmentId}/…`: the
//! same operations plus the attempt history of a delivery.
//!
//! Internal: `POST /internal/v1/platform/events` (`developer-event.publish`).
//!
//! Scope comes only from the confirmed Platform Access chain (machine) or the console
//! environment's own profile (human): another environment's subscription or delivery is
//! 404 without any existence hint. The signing secret is returned only by create /
//! rotate-secret. Disabled (404) unless `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED`.

use std::collections::HashMap;

use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::InternalPermission;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{Scope, WebhookChange, WebhookService};
use crate::{
    control::{
        self, activity, idempotency,
        pagination::{Page, PageQuery},
        stmt, Caller, Principal,
    },
    domain::rbac::Action,
    http::{
        error::{ApiError, ErrorCode},
        middleware::InternalCaller,
    },
    state::AppState,
};
use sea_orm::ConnectionTrait;

type ApiResult<T> = Result<T, ApiError>;

/// `/v1/**` routes (nested under `/v1` by the caller).
pub fn customer_routes() -> Router<AppState> {
    Router::new()
        .route("/webhooks", post(create).get(list))
        .route(
            "/webhooks/:webhook_id",
            get(get_one).patch(update).delete(delete),
        )
        .route("/webhooks/:webhook_id/rotate-secret", post(rotate))
        .route("/webhooks/:webhook_id/deliveries", get(deliveries))
        .route("/webhook-deliveries/:delivery_id/replay", post(replay))
        .route(
            "/environments/:environment_id/webhooks",
            get(console_list).post(console_create),
        )
        .route(
            "/environments/:environment_id/webhooks/:webhook_id",
            get(console_get)
                .patch(console_update)
                .delete(console_delete),
        )
        .route(
            "/environments/:environment_id/webhooks/:webhook_id/rotate-secret",
            post(console_rotate),
        )
        .route(
            "/environments/:environment_id/webhooks/:webhook_id/deliveries",
            get(console_deliveries),
        )
        .route(
            "/environments/:environment_id/webhook-deliveries/:delivery_id/attempts",
            get(console_attempts),
        )
        .route(
            "/environments/:environment_id/webhook-deliveries/:delivery_id/replay",
            post(console_replay),
        )
}

/// `/internal/v1/**` routes.
pub fn internal_routes() -> Router<AppState> {
    Router::new().route("/platform/events", post(ingest))
}

// ---------------------------------------------------------------------------
// Shared plumbing.
// ---------------------------------------------------------------------------

fn service(s: &AppState) -> ApiResult<&WebhookService> {
    s.webhooks
        .as_ref()
        .filter(|w| w.settings().enabled)
        .ok_or(ApiError(ErrorCode::NotFound))
}

/// A webhook response: never cached (secrets, endpoint URLs).
fn respond(status: StatusCode, body: Value) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn body<T: DeserializeOwned>(b: Result<Json<Value>, JsonRejection>) -> ApiResult<(T, Value)> {
    let Json(raw) = b.map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let parsed =
        serde_json::from_value(raw.clone()).map_err(|_| ApiError(ErrorCode::ValidationError))?;
    Ok((parsed, raw))
}

/// An id path segment: malformed is NOT_FOUND (as Calling answers).
fn id_path(value: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(value).map_err(|_| ApiError(ErrorCode::NotFound))
}

/// Query parameters, refusing unknown ones.
fn params(
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
    allowed: &[&str],
) -> ApiResult<HashMap<String, String>> {
    let Query(pairs) = q.map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let mut out = HashMap::new();
    for (k, v) in pairs {
        if !allowed.contains(&k.as_str()) || out.insert(k, v).is_some() {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    Ok(out)
}

fn page(p: &HashMap<String, String>) -> ApiResult<Page> {
    let limit = p
        .get("limit")
        .map(|l| {
            l.parse::<usize>()
                .map_err(|_| ApiError(ErrorCode::ValidationError))
        })
        .transpose()?;
    Page::from_query(&PageQuery {
        limit,
        cursor: p.get("cursor").cloned(),
    })
}

fn expected_version(p: &HashMap<String, String>) -> ApiResult<i64> {
    p.get("expectedVersion")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v >= 1)
        .ok_or(ApiError(ErrorCode::ValidationError))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBody {
    url: String,
    event_types: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateBody {
    expected_version: i64,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    event_types: Option<Vec<String>>,
    #[serde(default, with = "double_option")]
    description: Option<Option<String>>,
    #[serde(default)]
    status: Option<String>,
}

impl UpdateBody {
    fn change(self) -> ApiResult<(i64, WebhookChange)> {
        if self.url.is_none()
            && self.event_types.is_none()
            && self.description.is_none()
            && self.status.is_none()
        {
            return Err(ApiError(ErrorCode::ValidationError));
        }
        Ok((
            self.expected_version,
            WebhookChange {
                url: self.url,
                event_types: self.event_types,
                description: self.description,
                status: self.status,
            },
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBody {
    expected_version: i64,
}

/// `null` clears, absence keeps.
mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<String>>, D::Error> {
        Ok(Some(Option::<String>::deserialize(d)?))
    }
}

fn with_secret(webhook: Value, secret: String) -> Value {
    json!({ "webhook": webhook, "signingSecret": secret })
}

/// Removes the one-time secret from a stored idempotent response.
fn redact_secret(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        o.remove("signingSecret");
    }
}

// ---------------------------------------------------------------------------
// Machine (customer API) authorization.
// ---------------------------------------------------------------------------

/// The confirmed scope of a machine caller holding `action`'s scope (and `extra`).
///
/// A tenant without a Developer Platform organization (an existing Calling customer) is
/// served on its Platform Access authority alone; an organization's lifecycle state
/// applies when one exists.
async fn machine(s: &AppState, c: &Caller, action: Action, extra: &[&str]) -> ApiResult<Scope> {
    let Principal::Machine(m) = &c.principal else {
        // Humans use the console routes.
        return Err(ApiError(ErrorCode::PermissionDenied));
    };
    let mut scopes: Vec<&str> = control::machine_scope(action).into_iter().collect();
    scopes.extend_from_slice(extra);
    let allowed = s
        .platform_access
        .credential_allows(
            &m.delegated,
            &c.request.correlation_id,
            m.credential_id,
            &scopes,
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "identity authority unavailable");
            ApiError(ErrorCode::ServiceUnavailable)
        })?;
    let org = control::organization_state(&s.db, m.context.tenant_id).await?;
    let denied = if !allowed {
        Some(ErrorCode::InsufficientScope)
    } else {
        org.filter(|st| !st.permits(action))
            .map(|st| control::state_error(st).0)
    };
    if let Some(code) = denied {
        if org.is_some() && action.is_mutation() {
            activity::record_denied(&s.db, c, m.context.tenant_id, action, code).await;
        }
        return Err(ApiError(code));
    }
    Ok(Scope {
        tenant_id: m.context.tenant_id,
        application_id: m.context.application_id,
        environment_id: m.context.environment_id,
        environment_kind: m.context.environment_kind.clone(),
    })
}

/// Machine subscriptions additionally need the read scope of every event type.
async fn machine_types(s: &AppState, c: &Caller, types: &[String]) -> ApiResult<()> {
    let validated = WebhookService::validate_event_types(types)?;
    let needed = WebhookService::domain_scopes(&validated);
    if needed.is_empty() {
        return Ok(());
    }
    machine(s, c, Action::WebhooksManage, &needed)
        .await
        .map(|_| ())
}

fn list_body(rows: Vec<Value>, next: Option<String>) -> Value {
    json!({ "data": rows, "nextCursor": next })
}

async fn create(
    State(s): State<AppState>,
    c: Caller,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksManage, &[]).await?;
    let (b, _): (CreateBody, _) = body(b)?;
    svc.key_ring()?;
    svc.validate_url(&b.url)?;
    machine_types(&s, &c, &b.event_types).await?;
    let (row, secret) = svc
        .create(&c, &scope, &b.url, &b.event_types, b.description)
        .await?;
    Ok(respond(
        StatusCode::CREATED,
        with_secret(row.view(), secret),
    ))
}

async fn list(
    State(s): State<AppState>,
    c: Caller,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksRead, &[]).await?;
    let p = page(&params(q, &["limit", "cursor"])?)?;
    let l = svc.list(&scope, &p).await?;
    Ok(respond(
        StatusCode::OK,
        list_body(l.rows.iter().map(|r| r.view()).collect(), l.next_cursor),
    ))
}

async fn get_one(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksRead, &[]).await?;
    let row = svc.get(&scope, id_path(&id)?).await?;
    Ok(respond(StatusCode::OK, row.view()))
}

async fn update(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<String>,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksManage, &[]).await?;
    let id = id_path(&id)?;
    let (b, _): (UpdateBody, _) = body(b)?;
    if let Some(types) = &b.event_types {
        machine_types(&s, &c, types).await?;
    }
    let (version, change) = b.change()?;
    let row = svc.update(&c, &scope, id, version, change).await?;
    Ok(respond(StatusCode::OK, row.view()))
}

async fn delete(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<String>,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksManage, &[]).await?;
    let id = id_path(&id)?;
    let version = expected_version(&params(q, &["expectedVersion"])?)?;
    let row = svc.delete(&c, &scope, id, version).await?;
    Ok(respond(StatusCode::OK, row.view()))
}

async fn rotate(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<String>,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksManage, &[]).await?;
    let id = id_path(&id)?;
    let (b, _): (VersionBody, _) = body(b)?;
    let (row, secret) = svc
        .rotate_secret(&c, &scope, id, b.expected_version)
        .await?;
    Ok(respond(StatusCode::OK, with_secret(row.view(), secret)))
}

async fn deliveries(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<String>,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksRead, &[]).await?;
    let id = id_path(&id)?;
    let p = params(q, &["limit", "cursor", "status"])?;
    let l = svc
        .deliveries(&scope, id, p.get("status").map(String::as_str), &page(&p)?)
        .await?;
    Ok(respond(
        StatusCode::OK,
        list_body(l.rows.iter().map(|r| r.view()).collect(), l.next_cursor),
    ))
}

async fn replay(
    State(s): State<AppState>,
    c: Caller,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = machine(&s, &c, Action::WebhooksManage, &[]).await?;
    let row = svc.replay(&c, &scope, id_path(&id)?).await?;
    Ok(respond(StatusCode::CREATED, row.view()))
}

// ---------------------------------------------------------------------------
// Developer Console (human members).
// ---------------------------------------------------------------------------

/// The scope of a console environment for a human member allowed `action` (Section 50
/// matrix and organization state; non-members get NOT_FOUND).
async fn console(
    s: &AppState,
    c: &Caller,
    environment_id: &str,
    action: Action,
) -> ApiResult<Scope> {
    c.human()?;
    let environment_id = id_path(environment_id)?;
    let row =
        s.db.query_one(stmt(
            "SELECT tenant_id, application_id, kind FROM devplat_environment_profiles \
              WHERE environment_id = $1",
            vec![environment_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let tenant_id: Uuid = row.try_get("", "tenant_id")?;
    control::authorize(s, c, tenant_id, action).await?;
    Ok(Scope {
        tenant_id,
        application_id: row.try_get("", "application_id")?,
        environment_id,
        environment_kind: row.try_get("", "kind")?,
    })
}

fn console_list_body(rows: Vec<Value>, next: Option<String>) -> Value {
    json!({ "items": rows, "nextCursor": next })
}

async fn console_list(
    State(s): State<AppState>,
    c: Caller,
    Path(env): Path<String>,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksRead).await?;
    let p = page(&params(q, &["limit", "cursor"])?)?;
    let l = svc.list(&scope, &p).await?;
    Ok(respond(
        StatusCode::OK,
        console_list_body(
            l.rows.iter().map(|r| r.console_view()).collect(),
            l.next_cursor,
        ),
    ))
}

async fn console_create(
    State(s): State<AppState>,
    c: Caller,
    Path(env): Path<String>,
    headers: HeaderMap,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksManage).await?;
    let (b, raw): (CreateBody, _) = body(b)?;
    svc.key_ring()?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let idem_scope = format!("{}:{}:webhooks.create", c.actor_ref(), scope.environment_id);
    let digest = idempotency::digest(&[
        idem_scope.as_bytes(),
        &serde_json::to_vec(&raw).unwrap_or_default(),
    ]);
    let (status, out, replayed) = idempotency::run(
        &s.db,
        &idem_scope,
        key.as_deref(),
        &digest,
        redact_secret,
        || async {
            let (row, secret) = svc
                .create(&c, &scope, &b.url, &b.event_types, b.description.clone())
                .await?;
            Ok((StatusCode::CREATED, with_secret(row.console_view(), secret)))
        },
    )
    .await?;
    let mut r = respond(status, out);
    if replayed {
        r.headers_mut().insert(
            axum::http::HeaderName::from_static("idempotent-replayed"),
            HeaderValue::from_static("true"),
        );
    }
    Ok(r)
}

async fn console_get(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksRead).await?;
    let row = svc.get(&scope, id_path(&id)?).await?;
    Ok(respond(StatusCode::OK, row.console_view()))
}

async fn console_update(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksManage).await?;
    let id = id_path(&id)?;
    let (b, _): (UpdateBody, _) = body(b)?;
    let (version, change) = b.change()?;
    let row = svc.update(&c, &scope, id, version, change).await?;
    Ok(respond(StatusCode::OK, row.console_view()))
}

async fn console_delete(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksManage).await?;
    let id = id_path(&id)?;
    let version = expected_version(&params(q, &["expectedVersion"])?)?;
    let row = svc.delete(&c, &scope, id, version).await?;
    Ok(respond(StatusCode::OK, row.console_view()))
}

async fn console_rotate(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksManage).await?;
    let id = id_path(&id)?;
    let (b, _): (VersionBody, _) = body(b)?;
    let (row, secret) = svc
        .rotate_secret(&c, &scope, id, b.expected_version)
        .await?;
    Ok(respond(
        StatusCode::OK,
        with_secret(row.console_view(), secret),
    ))
}

async fn console_deliveries(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksRead).await?;
    let id = id_path(&id)?;
    let p = params(q, &["limit", "cursor", "status"])?;
    let l = svc
        .deliveries(&scope, id, p.get("status").map(String::as_str), &page(&p)?)
        .await?;
    Ok(respond(
        StatusCode::OK,
        console_list_body(l.rows.iter().map(|r| r.view()).collect(), l.next_cursor),
    ))
}

async fn console_attempts(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
    q: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksRead).await?;
    let id = id_path(&id)?;
    let p = page(&params(q, &["limit", "cursor"])?)?;
    let (rows, next) = svc.attempts(&scope, id, &p).await?;
    Ok(respond(
        StatusCode::OK,
        console_list_body(rows.iter().map(|r| r.view()).collect(), next),
    ))
}

async fn console_replay(
    State(s): State<AppState>,
    c: Caller,
    Path((env, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let svc = service(&s)?;
    let scope = console(&s, &c, &env, Action::WebhooksManage).await?;
    let row = svc.replay(&c, &scope, id_path(&id)?).await?;
    Ok(respond(StatusCode::CREATED, row.view()))
}

// ---------------------------------------------------------------------------
// Internal event ingestion.
// ---------------------------------------------------------------------------

/// `POST /internal/v1/platform/events` — a committed producer envelope. The producer is
/// the authenticated service (the envelope must say the same); 200 = accepted (a
/// duplicate returns the same event id; an internal-only fact has none); 400 = refused
/// (never retry); 503 = retry later (also while webhooks are not enabled).
async fn ingest(
    State(s): State<AppState>,
    caller: InternalCaller,
    b: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Response> {
    caller.require_permission(InternalPermission::DeveloperEventPublish)?;
    // Webhooks not (yet) enabled: retry later, never "refused" — producers keep the
    // committed event in their outbox instead of dead-lettering it before cut-over.
    let svc = service(&s).map_err(|_| ApiError(ErrorCode::ServiceUnavailable))?;
    let Json(envelope) = b.map_err(|_| ApiError(ErrorCode::ValidationError))?;
    let producer = caller
        .context()
        .authenticated_service()
        .as_str()
        .to_string();
    let runtime = s
        .service_auth
        .signer()
        .runtime_environment()
        .as_str()
        .to_string();
    let out = svc.ingest_envelope(&producer, &runtime, &envelope).await?;
    Ok(respond(
        StatusCode::OK,
        json!({ "status": "ACCEPTED", "eventId": out.event_id, "duplicate": out.duplicate }),
    ))
}
