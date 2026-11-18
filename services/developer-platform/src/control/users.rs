//! ADR 0062 — partner users and CF Meet user tokens (build contract §3).
//!
//! * **Users API** (`/v1/users*`, machine credentials through the API Gateway): every
//!   operation is delegated to Platform Access's Partner Users router (§2.1,
//!   `platform-end-users.delegate`) on behalf of the gateway-forwarded credential, which
//!   Platform Access re-verifies live and takes the environment from. Nothing about a
//!   partner user is stored here. The Developer Platform checks the credential's scope
//!   first (`principals.read`, `principals.write`, `user_tokens.issue`) like every machine
//!   route; Platform Access checks it again.
//! * **Console** (human sessions): `GET /v1/environments/{id}/users` (OWNER, ADMIN,
//!   DEVELOPER; `platform-identity.delegate`) and the SANDBOX development token generator
//!   `POST /v1/environments/{id}/test-user-tokens` (OWNER, ADMIN, DEVELOPER). A console
//!   session has no API credential to forward, so the generator uses the delegation route
//!   `POST /internal/v1/delegation/environments/{id}/test-user-tokens` (requested from
//!   Platform Access; same body as `/v1/users/tokens`).
//!
//! A user token is a bearer credential: it is returned once, never logged or stored, and
//! an idempotent replay returns the metadata WITHOUT it (`tokenRedacted: true`).
//!
//! Flags: `DEVELOPER_PLATFORM_USERS_API_ENABLED` (Users API and console list),
//! `DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED` (generator); off, the routes are 404.

use axum::http::StatusCode;
use sea_orm::TransactionTrait;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize, Caller, MachineCaller, Principal,
};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    platform_access::{Delegated, DelegationCall, EndUserCall, EndUsersReply, ForwardedCredential},
    state::AppState,
};

/// Most mint-able grants per meeting list (build contract §1).
const MAX_MEETING_GRANTS: usize = 10;
const MEETING_ROLES: &[&str] = &["HOST", "CO_HOST", "PRESENTER", "PARTICIPANT", "VIEWER"];

pub fn users_api_enabled(state: &AppState) -> Result<(), ApiError> {
    state
        .build
        .users_api
        .then_some(())
        .ok_or(ApiError(ErrorCode::NotFound))
}

pub fn test_tokens_enabled(state: &AppState) -> Result<(), ApiError> {
    state
        .build
        .test_tokens
        .then_some(())
        .ok_or(ApiError(ErrorCode::NotFound))
}

// ---------------------------------------------------------------------------------------
// Validation (Platform Access stays authoritative; these refuse early with 400).
// ---------------------------------------------------------------------------------------

/// `externalId`: 1–128 printable ASCII without spaces; an opaque id, never a phone number,
/// a `tel:` / `sip:` URI or a tombstone reference.
pub fn external_id(value: &str) -> Result<String, ApiError> {
    let b = value.as_bytes();
    let printable = (1..=128).contains(&b.len()) && b.iter().all(|c| (0x21..=0x7e).contains(c));
    let lower = value.to_ascii_lowercase();
    let plus_digit = b.windows(2).any(|w| w[0] == b'+' && w[1].is_ascii_digit());
    let digits = b.iter().filter(|c| c.is_ascii_digit()).count();
    let phone_like = digits >= 7
        && b.iter()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'(' | b')'));
    let uri_like = ["tel:", "sip:", "sips:", "tombstoned:"]
        .iter()
        .any(|p| lower.starts_with(p));
    if printable && !plus_digit && !phone_like && !uri_like {
        Ok(value.to_string())
    } else {
        Err(ApiError(ErrorCode::ValidationError))
    }
}

/// `displayName`: 1–64 characters, no control characters.
pub fn user_display_name(value: &str) -> Result<String, ApiError> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > 64 || v.chars().any(char::is_control) {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    Ok(v.to_string())
}

/// `imageUrl`: `https://` only, at most 512 characters.
pub fn image_url(value: &str) -> Result<String, ApiError> {
    let ok = value.len() <= 512
        && value.starts_with("https://")
        && value.len() > "https://".len()
        && !value.chars().any(|c| c.is_whitespace() || c.is_control());
    ok.then(|| value.to_string())
        .ok_or(ApiError(ErrorCode::ValidationError))
}

fn ttl(value: Option<i64>) -> Result<Option<i64>, ApiError> {
    match value {
        None => Ok(None),
        Some(t) if (60..=86_400).contains(&t) => Ok(Some(t)),
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

fn subset(value: &Value, allowed: &[&str]) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() <= allowed.len()
            && items
                .iter()
                .all(|i| i.as_str().is_some_and(|s| allowed.contains(&s)))
            && items
                .iter()
                .enumerate()
                .all(|(n, i)| !items[..n].contains(i))
    })
}

/// Grants (build contract §1): `msg` ⊆ {read, send}, `call` ⊆ {receive}, `mtg` at most 10
/// `{conferenceId, role}` entries (the token's compact `{c, r}` form is accepted too).
/// Forwarded verbatim; Platform Access intersects them with the credential's scopes.
pub fn grants(value: &Value) -> Result<(), ApiError> {
    let map = value
        .as_object()
        .ok_or(ApiError(ErrorCode::ValidationError))?;
    for (k, v) in map {
        let ok = match k.as_str() {
            "msg" => subset(v, &["read", "send"]),
            "call" => subset(v, &["receive"]),
            "mtg" => v.as_array().is_some_and(|entries| {
                entries.len() <= MAX_MEETING_GRANTS
                    && entries.iter().all(|e| {
                        let Some(o) = e.as_object() else {
                            return false;
                        };
                        let conference = o.get("conferenceId").or_else(|| o.get("c"));
                        let role = o.get("role").or_else(|| o.get("r"));
                        o.len() == 2
                            && conference
                                .and_then(Value::as_str)
                                .is_some_and(|c| Uuid::parse_str(c).is_ok())
                            && role
                                .and_then(Value::as_str)
                                .is_some_and(|r| MEETING_ROLES.contains(&r))
                    })
            }),
            _ => false,
        };
        if !ok {
            return Err(ApiError(ErrorCode::ValidationError));
        }
    }
    Ok(())
}

fn cursor(value: Option<String>) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(c)
            if (1..=256).contains(&c.len()) && c.bytes().all(|b| (0x21..=0x7e).contains(&b)) =>
        {
            Ok(Some(c))
        }
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

fn limit(value: Option<u32>) -> Result<Option<u32>, ApiError> {
    match value {
        None => Ok(None),
        Some(l) if (1..=100).contains(&l) => Ok(Some(l)),
        Some(_) => Err(ApiError(ErrorCode::ValidationError)),
    }
}

// ---------------------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateUser {
    pub external_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
}

impl CreateUser {
    fn forwarded(&self) -> Result<Value, ApiError> {
        let mut body = Map::new();
        body.insert("externalId".into(), json!(external_id(&self.external_id)?));
        if let Some(n) = &self.display_name {
            body.insert("displayName".into(), json!(user_display_name(n)?));
        }
        if let Some(u) = &self.image_url {
            body.insert("imageUrl".into(), json!(image_url(u)?));
        }
        Ok(Value::Object(body))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueToken {
    pub grants: Value,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

impl IssueToken {
    fn forwarded(&self) -> Result<Value, ApiError> {
        grants(&self.grants)?;
        let mut body = Map::new();
        body.insert("grants".into(), self.grants.clone());
        if let Some(t) = ttl(self.ttl_seconds)? {
            body.insert("ttlSeconds".into(), json!(t));
        }
        Ok(Value::Object(body))
    }
}

/// `POST /v1/users/tokens` and the console generator: upsert the user, then mint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpsertToken {
    pub external_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    pub grants: Value,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

impl UpsertToken {
    pub fn forwarded(&self) -> Result<Value, ApiError> {
        let mut body = CreateUser {
            external_id: self.external_id.clone(),
            display_name: self.display_name.clone(),
            image_url: self.image_url.clone(),
        }
        .forwarded()?;
        let token = IssueToken {
            grants: self.grants.clone(),
            ttl_seconds: self.ttl_seconds,
        }
        .forwarded()?;
        if let (Some(b), Some(t)) = (body.as_object_mut(), token.as_object()) {
            b.extend(t.clone());
        }
        Ok(body)
    }
}

/// `PATCH /v1/users/{userId}`: `displayName` / `imageUrl` (a string, or `null` to clear)
/// and `status` (`ACTIVE` / `SUSPENDED`); at least one.
pub fn update_body(value: &Value) -> Result<Value, ApiError> {
    let map = value
        .as_object()
        .filter(|m| !m.is_empty())
        .ok_or(ApiError(ErrorCode::ValidationError))?;
    let mut out = Map::new();
    for (k, v) in map {
        let checked = match (k.as_str(), v) {
            ("displayName" | "imageUrl", Value::Null) => Value::Null,
            ("displayName", Value::String(s)) => json!(user_display_name(s)?),
            ("imageUrl", Value::String(s)) => json!(image_url(s)?),
            ("status", Value::String(s)) if matches!(s.as_str(), "ACTIVE" | "SUSPENDED") => {
                v.clone()
            }
            _ => return Err(ApiError(ErrorCode::ValidationError)),
        };
        out.insert(k.clone(), checked);
    }
    Ok(Value::Object(out))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListUsersQuery {
    pub external_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsoleUsersQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------------------
// Views (only a well-formed authority answer is shown)
// ---------------------------------------------------------------------------------------

fn nullable_string(v: Option<&Value>) -> Option<Value> {
    match v {
        None | Some(Value::Null) => Some(Value::Null),
        Some(Value::String(s)) => Some(json!(s)),
        Some(_) => None,
    }
}

/// The partner user (build contract §2.1 user object). Platform Access may wrap it as
/// `{user: {...}}`.
pub fn user_view(pa: &Value) -> Result<Value, ApiError> {
    let u = pa.get("user").filter(|u| u.is_object()).unwrap_or(pa);
    let id = u
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let external = u.get("externalId").and_then(Value::as_str);
    let status = u
        .get("status")
        .and_then(Value::as_str)
        .filter(|s| matches!(*s, "ACTIVE" | "SUSPENDED" | "DEACTIVATED"));
    let display = nullable_string(u.get("displayName"));
    let image = nullable_string(u.get("imageUrl"));
    match (id, external, status, display, image) {
        (Some(id), Some(external), Some(status), Some(display), Some(image)) => Ok(json!({
            "id": id,
            "externalId": external,
            "displayName": display,
            "imageUrl": image,
            "status": status,
            "createdAt": u.get("createdAt").cloned().unwrap_or(Value::Null),
            "updatedAt": u.get("updatedAt").cloned().unwrap_or(Value::Null),
        })),
        _ => {
            tracing::warn!("platform access returned a malformed partner user");
            Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    }
}

fn list_view(pa: &Value) -> Result<Value, ApiError> {
    let items = pa
        .get("items")
        .and_then(Value::as_array)
        .ok_or(ApiError(ErrorCode::ServiceUnavailable))?
        .iter()
        .map(user_view)
        .collect::<Result<Vec<_>, _>>()?;
    let next = match pa.get("nextCursor") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => json!(s),
        Some(_) => return Err(ApiError(ErrorCode::ServiceUnavailable)),
    };
    Ok(json!({ "items": items, "nextCursor": next }))
}

/// `{token, tokenId, expiresAt}` (plus `user` for an upsert).
fn token_view(pa: &Value, with_user: bool) -> Result<Value, ApiError> {
    let token = pa
        .get("token")
        .and_then(Value::as_str)
        .filter(|t| t.starts_with("cfut_") && t.len() <= 2048);
    let token_id = pa
        .get("tokenId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let expires = pa.get("expiresAt").and_then(Value::as_str);
    let (Some(token), Some(token_id), Some(expires)) = (token, token_id, expires) else {
        tracing::warn!("platform access returned a malformed user token");
        return Err(ApiError(ErrorCode::ServiceUnavailable));
    };
    let mut out = json!({ "token": token, "tokenId": token_id, "expiresAt": expires });
    if with_user {
        out["user"] = user_view(
            pa.get("user")
                .ok_or(ApiError(ErrorCode::ServiceUnavailable))?,
        )?;
    }
    Ok(out)
}

/// The stored (replayable) copy of a token response: never the token itself.
pub fn redact_token(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        if o.remove("token").is_some() {
            o.insert("tokenRedacted".into(), Value::Bool(true));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Platform Access
// ---------------------------------------------------------------------------------------

/// Platform Access's Partner Users refusals as Developer Platform errors.
pub fn refusal(status: u16, code: &str) -> ApiError {
    ApiError(match (status, code) {
        (404, _) => ErrorCode::NotFound,
        (_, "LIMIT_EXCEEDED") => ErrorCode::LimitExceeded,
        (_, "SCOPE_REQUIRED" | "INSUFFICIENT_SCOPE") => ErrorCode::InsufficientScope,
        (429, _) | (_, "RATE_LIMITED") => ErrorCode::RateLimited,
        (_, "POLICY_VIOLATION") => ErrorCode::PolicyViolation,
        (_, "VALIDATION_ERROR") | (400 | 422, _) => ErrorCode::ValidationError,
        (409, _) | (_, "INVALID_STATE") => ErrorCode::InvalidState,
        (403, _) => ErrorCode::PermissionDenied,
        _ => ErrorCode::ServiceUnavailable,
    })
}

fn machine(caller: &Caller) -> Result<&MachineCaller, ApiError> {
    match &caller.principal {
        Principal::Machine(m) => Ok(m),
        // The Users API is server-to-server (API credentials); the console reads users
        // through `/v1/environments/{id}/users`.
        Principal::Human(_) => Err(ApiError(ErrorCode::PermissionDenied)),
    }
}

/// Checks the flag, the caller (machine), the organization and the credential's scope.
async fn machine_access<'a>(
    state: &AppState,
    caller: &'a Caller,
    action: Action,
) -> Result<&'a MachineCaller, ApiError> {
    users_api_enabled(state)?;
    let m = machine(caller)?;
    authorize(state, caller, m.context.tenant_id, action).await?;
    Ok(m)
}

async fn call(
    state: &AppState,
    caller: &Caller,
    m: &MachineCaller,
    call: EndUserCall,
) -> Result<(u16, Value), ApiError> {
    let forwarded = ForwardedCredential {
        delegated: m.delegated.clone(),
        credential_id: m.credential_id,
    };
    match state
        .platform_access
        .end_users(call, &forwarded, &caller.request.correlation_id)
        .await
    {
        Ok(EndUsersReply::Ok { status, body }) => Ok((status, body)),
        Ok(EndUsersReply::Refused { status, code }) => {
            tracing::info!(status, %code, "platform access refused a partner users operation");
            Err(refusal(status, &code))
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    }
}

fn status_code(status: u16, default: StatusCode) -> StatusCode {
    match status {
        200 => StatusCode::OK,
        201 => StatusCode::CREATED,
        _ => default,
    }
}

// ---------------------------------------------------------------------------------------
// Users API (machine)
// ---------------------------------------------------------------------------------------

/// `POST /v1/users` — `201` new, `200` existing (idempotent on `externalId`).
pub async fn create(
    state: &AppState,
    caller: &Caller,
    body: CreateUser,
) -> Result<(StatusCode, Value), ApiError> {
    let m = machine_access(state, caller, Action::EndUsersManage).await?;
    let body = body.forwarded()?;
    let (status, pa) = call(state, caller, m, EndUserCall::Create { body }).await?;
    Ok((status_code(status, StatusCode::CREATED), user_view(&pa)?))
}

/// `GET /v1/users`.
pub async fn list(state: &AppState, caller: &Caller, q: ListUsersQuery) -> Result<Value, ApiError> {
    let m = machine_access(state, caller, Action::EndUsersRead).await?;
    let external = q.external_id.as_deref().map(external_id).transpose()?;
    let (_, pa) = call(
        state,
        caller,
        m,
        EndUserCall::List {
            external_id: external,
            cursor: cursor(q.cursor)?,
            limit: limit(q.limit)?,
        },
    )
    .await?;
    list_view(&pa)
}

/// `GET /v1/users/{userId}`.
pub async fn get(state: &AppState, caller: &Caller, user_id: Uuid) -> Result<Value, ApiError> {
    let m = machine_access(state, caller, Action::EndUsersRead).await?;
    let (_, pa) = call(state, caller, m, EndUserCall::Get { user_id }).await?;
    user_view(&pa)
}

/// `PATCH /v1/users/{userId}`.
pub async fn update(
    state: &AppState,
    caller: &Caller,
    user_id: Uuid,
    body: &Value,
) -> Result<Value, ApiError> {
    let m = machine_access(state, caller, Action::EndUsersManage).await?;
    let body = update_body(body)?;
    let (_, pa) = call(state, caller, m, EndUserCall::Update { user_id, body }).await?;
    user_view(&pa)
}

/// `DELETE /v1/users/{userId}` — tombstone (profile erased, ids kept).
pub async fn delete(state: &AppState, caller: &Caller, user_id: Uuid) -> Result<(), ApiError> {
    let m = machine_access(state, caller, Action::EndUsersManage).await?;
    call(state, caller, m, EndUserCall::Delete { user_id }).await?;
    Ok(())
}

/// `POST /v1/users/{userId}/revoke-sessions` — `{tokensNotBefore}`.
pub async fn revoke_sessions(
    state: &AppState,
    caller: &Caller,
    user_id: Uuid,
) -> Result<Value, ApiError> {
    let m = machine_access(state, caller, Action::EndUsersManage).await?;
    let (_, pa) = call(state, caller, m, EndUserCall::RevokeSessions { user_id }).await?;
    match pa.get("tokensNotBefore").and_then(Value::as_str) {
        Some(t) => Ok(json!({ "tokensNotBefore": t })),
        None => Err(ApiError(ErrorCode::ServiceUnavailable)),
    }
}

/// `POST /v1/users/{userId}/tokens` — `201 {token, tokenId, expiresAt}`.
pub async fn issue_token(
    state: &AppState,
    caller: &Caller,
    user_id: Uuid,
    body: IssueToken,
) -> Result<(StatusCode, Value), ApiError> {
    let m = machine_access(state, caller, Action::UserTokensIssue).await?;
    let body = body.forwarded()?;
    let (_, pa) = call(state, caller, m, EndUserCall::IssueToken { user_id, body }).await?;
    Ok((StatusCode::CREATED, token_view(&pa, false)?))
}

/// `POST /v1/users/tokens` — upsert the user and mint (`principals.write` +
/// `user_tokens.issue`): `201 {user, token, tokenId, expiresAt}`.
pub async fn upsert_token(
    state: &AppState,
    caller: &Caller,
    body: UpsertToken,
) -> Result<(StatusCode, Value), ApiError> {
    let m = machine_access(state, caller, Action::UserTokensIssue).await?;
    // The second scope of the upsert (the first was checked by `authorize`).
    let writes = state
        .platform_access
        .credential_allows(
            &m.delegated,
            &caller.request.correlation_id,
            m.credential_id,
            &["principals.write"],
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "identity authority unavailable");
            ApiError(ErrorCode::ServiceUnavailable)
        })?;
    if !writes {
        return Err(ApiError(ErrorCode::InsufficientScope));
    }
    let body = body.forwarded()?;
    let (_, pa) = call(state, caller, m, EndUserCall::UpsertToken { body }).await?;
    Ok((StatusCode::CREATED, token_view(&pa, true)?))
}

// ---------------------------------------------------------------------------------------
// Console (human sessions)
// ---------------------------------------------------------------------------------------

/// A console delegation (`platform-identity.delegate`) of the ADR 0062 routes, with the
/// Partner Users refusal mapping.
pub(crate) async fn console_delegate(
    state: &AppState,
    caller: &Caller,
    call: DelegationCall,
) -> Result<Value, ApiError> {
    match state
        .platform_access
        .delegate(call, &caller.request.correlation_id)
        .await
    {
        Ok(Delegated::Ok(v)) => Ok(v),
        Ok(Delegated::Refused { status, code }) => {
            tracing::info!(status, %code, "platform access refused a console delegation");
            Err(refusal(status, &code))
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            Err(ApiError(ErrorCode::ServiceUnavailable))
        }
    }
}

/// `GET /v1/environments/{environmentId}/users` — the environment's partner users.
pub async fn console_list(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    q: ConsoleUsersQuery,
) -> Result<Value, ApiError> {
    users_api_enabled(state)?;
    caller.human()?;
    let env = super::service_accounts::environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::EndUsersRead).await?;
    let pa = console_delegate(
        state,
        caller,
        DelegationCall::ListEnvironmentEndUsers {
            environment_id,
            cursor: cursor(q.cursor)?,
            limit: limit(q.limit)?,
        },
    )
    .await?;
    list_view(&pa)
}

/// `POST /v1/environments/{environmentId}/test-user-tokens` — a development token for a
/// SANDBOX environment (same body and answer as `/v1/users/tokens`).
pub async fn console_test_token(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    body: UpsertToken,
) -> Result<(StatusCode, Value), ApiError> {
    test_tokens_enabled(state)?;
    caller.human()?;
    let env = super::service_accounts::environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::UserTokensIssue).await?;
    if env.kind != "SANDBOX" {
        return Err(ApiError(ErrorCode::PolicyViolation));
    }
    if env.status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let forwarded = body.forwarded()?;
    let pa = console_delegate(
        state,
        caller,
        DelegationCall::IssueTestUserToken {
            actor: caller.delegation_actor(),
            environment_id,
            body: forwarded,
        },
    )
    .await?;
    let v = token_view(&pa, true)?;
    let txn = state.db.begin().await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: "user_token.test_issued",
            target_type: "ENVIRONMENT",
            target_id: environment_id.to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id,
                "userId": v["user"]["id"], "expiresAt": v["expiresAt"],
            }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ids_are_opaque() {
        for ok in ["rider-1", "user_42", "a", "u:abc.def", &"x".repeat(128)] {
            assert!(external_id(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "has space",
            "+15551234567",
            "id+1555",
            "555-123-4567",
            "tel:123",
            "SIP:alice@example.com",
            "tombstoned:x",
            &"x".repeat(129),
            "caf\u{e9}",
        ] {
            assert!(external_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn profiles_and_grants_are_bounded() {
        assert!(user_display_name("Rider One").is_ok());
        assert!(user_display_name(&"x".repeat(65)).is_err());
        assert!(user_display_name("a\u{7}").is_err());
        assert!(image_url("https://cdn.example.com/a.png").is_ok());
        assert!(image_url("http://cdn.example.com/a.png").is_err());
        assert!(image_url(&format!("https://{}", "x".repeat(510))).is_err());
        let c = Uuid::new_v4();
        assert!(grants(&json!({})).is_ok());
        assert!(
            grants(&json!({ "msg": ["read", "send"], "call": ["receive"],
            "mtg": [{ "conferenceId": c, "role": "PARTICIPANT" }, { "c": c, "r": "HOST" }] }))
            .is_ok()
        );
        for bad in [
            json!([]),
            json!({ "msg": ["write"] }),
            json!({ "msg": ["read", "read"] }),
            json!({ "call": ["place"] }),
            json!({ "mtg": [{ "conferenceId": "nope", "role": "HOST" }] }),
            json!({ "mtg": [{ "conferenceId": c, "role": "OWNER" }] }),
            json!({ "mtg": [{ "conferenceId": c, "role": "HOST", "x": 1 }] }),
            json!({ "mtg": vec![json!({ "c": c, "r": "VIEWER" }); 11] }),
            json!({ "sms": ["send"] }),
        ] {
            assert!(grants(&bad).is_err(), "{bad}");
        }
        assert!(ttl(Some(59)).is_err() && ttl(Some(86_401)).is_err());
        assert_eq!(ttl(Some(60)).unwrap(), Some(60));
    }

    #[test]
    fn updates_accept_only_profile_fields_and_status() {
        assert!(update_body(&json!({})).is_err());
        assert!(update_body(&json!({ "status": "DEACTIVATED" })).is_err());
        assert!(update_body(&json!({ "externalId": "x" })).is_err());
        let v = update_body(&json!({ "displayName": null, "status": "SUSPENDED" })).unwrap();
        assert_eq!(v, json!({ "displayName": null, "status": "SUSPENDED" }));
    }

    #[test]
    fn authority_answers_are_checked_and_tokens_redacted() {
        let id = Uuid::new_v4();
        let user = json!({ "id": id, "externalId": "rider-1", "displayName": null,
            "imageUrl": null, "status": "ACTIVE", "createdAt": "2026-11-18T10:00:00Z",
            "updatedAt": "2026-11-18T10:00:00Z", "internal": "x" });
        let v = user_view(&user).unwrap();
        assert!(v.get("internal").is_none());
        assert_eq!(user_view(&json!({ "user": user.clone() })).unwrap(), v);
        let mut bad = user.clone();
        bad["status"] = json!("GONE");
        assert!(user_view(&bad).is_err());
        let mut t = token_view(
            &json!({ "token": "cfut_sbx_x.y.z", "tokenId": Uuid::new_v4(),
                "expiresAt": "2026-11-18T11:00:00Z", "user": user }),
            true,
        )
        .unwrap();
        assert_eq!(t["user"]["id"], json!(id));
        redact_token(&mut t);
        assert!(t.get("token").is_none());
        assert_eq!(t["tokenRedacted"], true);
        assert!(token_view(&json!({ "token": "not-a-user-token" }), false).is_err());
    }

    #[test]
    fn refusals_map_to_stable_errors() {
        assert_eq!(refusal(404, "NOT_FOUND").0, ErrorCode::NotFound);
        assert_eq!(refusal(409, "LIMIT_EXCEEDED").0, ErrorCode::LimitExceeded);
        assert_eq!(
            refusal(422, "VALIDATION_ERROR").0,
            ErrorCode::ValidationError
        );
        assert_eq!(
            refusal(403, "SCOPE_REQUIRED").0,
            ErrorCode::InsufficientScope
        );
        assert_eq!(refusal(429, "RATE_LIMITED").0, ErrorCode::RateLimited);
        assert_eq!(
            refusal(403, "POLICY_VIOLATION").0,
            ErrorCode::PolicyViolation
        );
        assert_eq!(refusal(500, "BOOM").0, ErrorCode::ServiceUnavailable);
    }
}
