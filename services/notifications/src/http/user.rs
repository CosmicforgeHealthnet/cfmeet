//! I2 `dispatchNotificationsUser` (Document 4 §3.1): the Meeting Service relay verifies the
//! Account session and forwards the user request with the server-resolved account. The
//! answer carries the user operation's status and a body with `schemaVersion: 1`, which
//! the relay checks before returning it. Path allowlist: U1–U9 only.
//!
//! | Op | Relayed request |
//! |---|---|
//! | U1 | `GET` `` (or `/`) `?cursor&limit&state=unread|all&category=A,B` |
//! | U2 | `GET /unread-count` |
//! | U3 | `PATCH /{notificationId}` |
//! | U4 | `POST /read` |
//! | U5 | `POST /dismiss` |
//! | U6 | `GET /preferences` |
//! | U7 | `PUT /preferences` |
//! | U8 | `PUT /devices/{accountDeviceId}/push` |
//! | U9 | `DELETE /devices/{accountDeviceId}/push?platform=` |
//!
//! Mutations (U3, U4, U5, U7, U8) require an `Idempotency-Key`; every operation has a
//! per-account rate limit (per replica).

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use cf_service_auth::InternalPermission;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{middleware::Caller, parse};
use crate::{
    error::{error_body, ApiError, ApiResult, ErrorCode},
    inbox, preferences, push,
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserDispatch {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: Option<String>,
    pub account: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub body: Option<Value>,
}

fn account_ref_ok(r: &str) -> bool {
    r.strip_prefix("cfmeet-user:")
        .and_then(|u| Uuid::parse_str(u).ok())
        .is_some_and(|u| format!("cfmeet-user:{u}") == r)
}

fn query_map(q: Option<&str>) -> HashMap<String, String> {
    q.unwrap_or("")
        .trim_start_matches('?')
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (
                k.to_string(),
                v.replace("%2C", ",")
                    .replace("%3A", ":")
                    .replace("%2B", "+"),
            )
        })
        .collect()
}

type Windows = Mutex<HashMap<(String, &'static str), (i64, u32)>>;
static WINDOWS: OnceLock<Windows> = OnceLock::new();

/// Fixed one-minute window per (account, operation). Returns seconds to wait when over.
fn rate_limited(account: &str, op: &'static str, per_minute: u32) -> Option<u32> {
    let now = Utc::now().timestamp();
    let minute = now / 60;
    let mut w = WINDOWS
        .get_or_init(Default::default)
        .lock()
        .expect("rate windows");
    if w.len() > 100_000 {
        w.retain(|_, (m, _)| *m == minute);
    }
    let entry = w.entry((account.to_string(), op)).or_insert((minute, 0));
    if entry.0 != minute {
        *entry = (minute, 0);
    }
    entry.1 += 1;
    (entry.1 > per_minute).then(|| (60 - (now % 60)) as u32)
}

fn relay(status: StatusCode, mut body: Value) -> Response {
    if let Some(o) = body.as_object_mut() {
        o.insert("schemaVersion".into(), json!(1));
    }
    (status, Json(body)).into_response()
}

fn relay_error(e: ApiError) -> Response {
    let mut r = (
        e.code.status(),
        Json(error_body(e.code, e.message.as_deref())),
    )
        .into_response();
    if let Some(s) = e.retry_after {
        if let Ok(v) = axum::http::HeaderValue::from_str(&s.to_string()) {
            r.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
        }
    }
    r
}

pub async fn dispatch(
    State(state): State<AppState>,
    caller: Caller,
    body: Bytes,
) -> ApiResult<Response> {
    caller.require(InternalPermission::NotificationsUserForward)?;
    let d: UserDispatch = parse(&body)?;
    if !account_ref_ok(&d.account) {
        return Err(ApiError::validation("account must be cfmeet-user:<uuid>"));
    }
    Ok(match handle(&state, &d).await {
        Ok(r) => r,
        Err(e) => relay_error(e),
    })
}

fn body_of<T: for<'de> Deserialize<'de>>(d: &UserDispatch) -> ApiResult<T> {
    let v = d.body.clone().unwrap_or(json!({}));
    if !v.is_object() {
        return Err(ApiError::validation(
            "The request body must be a JSON object.",
        ));
    }
    serde_json::from_value(v).map_err(|_| {
        ApiError::validation("The request body has a missing, unknown or mistyped field.")
    })
}

fn need_key(d: &UserDispatch) -> ApiResult<()> {
    match d.idempotency_key.as_deref() {
        Some(k) if (8..=128).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_graphic()) => {
            Ok(())
        }
        _ => Err(ApiError::validation("Idempotency-Key is required")),
    }
}

fn limit(account: &str, op: &'static str, per_minute: u32) -> ApiResult<()> {
    match rate_limited(account, op, per_minute) {
        Some(s) => Err(ApiError::rate_limited(s)),
        None => Ok(()),
    }
}

async fn handle(state: &AppState, d: &UserDispatch) -> ApiResult<Response> {
    let path = d.path.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let q = query_map(d.query.as_deref());
    let account = d.account.as_str();
    match (d.method.as_str(), segments.as_slice()) {
        ("GET", []) => {
            limit(account, "U1", 120)?;
            let lim = q
                .get("limit")
                .map(|l| {
                    l.parse::<i64>()
                        .map_err(|_| ApiError::validation("limit is 1-100"))
                })
                .transpose()?
                .unwrap_or(30);
            if !(1..=100).contains(&lim) {
                return Err(ApiError::validation("limit is 1-100"));
            }
            let unread_only = match q.get("state").map(String::as_str) {
                None | Some("all") => false,
                Some("unread") => true,
                Some(_) => return Err(ApiError::validation("state is unread or all")),
            };
            let categories: Vec<String> = q
                .get("category")
                .map(|c| {
                    c.split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if categories.len() > 14
                || categories
                    .iter()
                    .any(|c| !c.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
            {
                return Err(ApiError::validation("category is a catalogue category"));
            }
            let cursor = match q.get("cursor") {
                Some(c) => Some(
                    inbox::decode_cursor(c)
                        .ok_or_else(|| ApiError::validation("invalid cursor"))?,
                ),
                None => None,
            };
            let v = inbox::list(
                &state.db,
                account,
                &inbox::ListQuery {
                    cursor,
                    limit: lim,
                    unread_only,
                    categories,
                },
            )
            .await?;
            Ok(relay(StatusCode::OK, v))
        }
        ("GET", ["unread-count"]) => {
            limit(account, "U2", 240)?;
            let (unread, latest, version) = inbox::summary(&state.db, account).await?;
            let mut r = relay(
                StatusCode::OK,
                json!({ "unreadCount": unread, "latestCreatedAt": latest, "version": version }),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str(&format!("W/\"{unread}-{version}\"")) {
                r.headers_mut().insert(axum::http::header::ETAG, v);
            }
            Ok(r)
        }
        ("PATCH", [id]) if *id != "read" && *id != "dismiss" && *id != "preferences" => {
            limit(account, "U3", 60)?;
            need_key(d)?;
            let id = Uuid::parse_str(id).map_err(|_| ApiError::not_found())?;
            let u: inbox::ItemUpdate = body_of(d)?;
            if u.read.is_none() && u.dismissed.is_none() {
                return Err(ApiError::validation("read or dismissed is required"));
            }
            match inbox::update(&state.db, account, id, &u).await? {
                inbox::UpdateOutcome::Updated(v) => Ok(relay(StatusCode::OK, *v)),
                inbox::UpdateOutcome::NotFound => Err(ApiError::not_found()),
                inbox::UpdateOutcome::VersionConflict => {
                    Err(ApiError::new(ErrorCode::VersionConflict))
                }
            }
        }
        ("POST", [op @ ("read" | "dismiss")]) => {
            let dismiss = *op == "dismiss";
            limit(account, if dismiss { "U5" } else { "U4" }, 30)?;
            need_key(d)?;
            let req: inbox::BulkRequest = body_of(d)?;
            req.validate().map_err(ApiError::validation)?;
            if !dismiss && req.category.is_some() {
                return Err(ApiError::validation("category applies to dismiss only"));
            }
            let n = inbox::bulk(&state.db, account, &req, dismiss).await?;
            let unread = inbox::unread_count(&state.db, account).await?;
            let key = if dismiss { "dismissed" } else { "updated" };
            Ok(relay(
                StatusCode::OK,
                json!({ key: n, "unreadCount": unread }),
            ))
        }
        ("GET", ["preferences"]) => {
            limit(account, "U6", 60)?;
            let tz = match state.authorities.contacts(&[account.to_string()]).await {
                Ok(c) => c.get(account).and_then(|c| c.time_zone.clone()),
                Err(_) => None,
            };
            let v = preferences::document(&state.db, account, tz.as_deref()).await?;
            Ok(relay(StatusCode::OK, v))
        }
        ("PUT", ["preferences"]) => {
            limit(account, "U7", 30)?;
            need_key(d)?;
            let u: preferences::PreferencesUpdate = body_of(d)?;
            match preferences::update(&state.db, account, &u, Utc::now()).await {
                Ok(_) => {
                    let tz = state
                        .authorities
                        .contacts(&[account.to_string()])
                        .await
                        .ok()
                        .and_then(|c| c.get(account).and_then(|c| c.time_zone.clone()));
                    Ok(relay(
                        StatusCode::OK,
                        preferences::document(&state.db, account, tz.as_deref()).await?,
                    ))
                }
                Err(preferences::UpdateError::Invalid(m)) => Err(ApiError::validation(m)),
                Err(preferences::UpdateError::VersionConflict) => {
                    Err(ApiError::new(ErrorCode::VersionConflict))
                }
                Err(preferences::UpdateError::Db(e)) => Err(e.0.into()),
            }
        }
        ("PUT", ["devices", device, "push"]) => {
            limit(account, "U8", 10)?;
            need_key(d)?;
            let device = Uuid::parse_str(device).map_err(|_| ApiError::not_found())?;
            let req: push::RegisterRequest = body_of(d)?;
            // ADR 0060: only the caller's ACTIVE Account device may register.
            let contacts = state
                .authorities
                .contacts(&[account.to_string()])
                .await
                .map_err(|_| ApiError::unavailable(5))?;
            let owns = contacts.get(account).is_some_and(|c| {
                c.active()
                    && c.devices
                        .iter()
                        .any(|x| x.account_device_ref == device.to_string() && x.state == "ACTIVE")
            });
            if !owns {
                return Err(ApiError::not_found());
            }
            let provider = state.push.as_deref();
            match push::register(
                &state.db,
                &state.keys,
                provider,
                account,
                device,
                &req,
                state.local_test_push_allowed(),
            )
            .await
            {
                Ok(target) => Ok(relay(
                    StatusCode::OK,
                    json!({ "pushTargetRef": target, "platform": req.platform, "enabled": true }),
                )),
                Err(push::RegisterError::Invalid(m)) => Err(ApiError::validation(m)),
                Err(push::RegisterError::Db) => Err(ApiError::unavailable(5)),
            }
        }
        ("DELETE", ["devices", device, "push"]) => {
            limit(account, "U9", 10)?;
            let device = Uuid::parse_str(device).map_err(|_| ApiError::not_found())?;
            let platform = q.get("platform").map(String::as_str);
            if platform.is_some_and(|p| !push::PLATFORMS.contains(&p)) {
                return Err(ApiError::validation(
                    "platform is APNS, APNS_VOIP, FCM or WEB_PUSH",
                ));
            }
            push::remove(&state.db, account, device, platform).await?;
            Ok(relay(StatusCode::OK, json!({ "removed": true })))
        }
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_refs_and_queries_are_strict() {
        let u = Uuid::new_v4();
        assert!(account_ref_ok(&format!("cfmeet-user:{u}")));
        assert!(!account_ref_ok(&format!("cfmeet-user:{}", u.simple())));
        assert!(!account_ref_ok("platform-principal:x"));
        let q = query_map(Some(
            "state=unread&category=CONTACT_REQUESTS%2CMISSED_CALLS&limit=5",
        ));
        assert_eq!(q["category"], "CONTACT_REQUESTS,MISSED_CALLS");
        assert_eq!(q["limit"], "5");
    }

    #[test]
    fn rate_limits_are_per_account_and_operation() {
        let a = format!("cfmeet-user:{}", Uuid::new_v4());
        for _ in 0..3 {
            assert!(rate_limited(&a, "T", 3).is_none());
        }
        assert!(rate_limited(&a, "T", 3).is_some());
        assert!(rate_limited(&a, "T2", 3).is_none());
    }
}
