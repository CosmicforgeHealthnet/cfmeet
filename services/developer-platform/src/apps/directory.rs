//! ADR 0065 — the anonymous app directory: PUBLISHED, PUBLIC, not suspended apps only,
//! shown with their PUBLISHED manifest (later unreviewed changes never appear).
//!
//! * `GET /v1/app-directory?limit&cursor` → `{items, nextCursor}`;
//! * `GET /v1/app-directory/{appId}` (anything not listed: 404).

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    control::{pagination::Page, stmt},
    http::error::{ApiError, ErrorCode},
    state::AppState,
};

const LISTED: &str = "a.published_version IS NOT NULL AND a.status <> 'SUSPENDED' \
    AND v.manifest ->> 'distribution' = 'PUBLIC'";

const SELECT: &str = "SELECT a.app_id, a.published_at, v.manifest, o.display_name AS developer \
    FROM devplat_apps a \
    JOIN devplat_app_versions v ON v.app_id = a.app_id AND v.version = a.published_version \
    JOIN devplat_organizations o ON o.tenant_id = a.tenant_id";

fn entry(r: &sea_orm::QueryResult) -> Result<(Value, DateTime<Utc>, Uuid), ApiError> {
    let id: Uuid = r.try_get("", "app_id")?;
    let at: DateTime<Utc> = r.try_get("", "published_at")?;
    let m: Value = r.try_get("", "manifest")?;
    let developer: String = r.try_get("", "developer")?;
    Ok((
        json!({
            "appId": id,
            "name": m["name"],
            "description": m["description"],
            "logoUrl": m["logoUrl"],
            "developerName": developer,
            "userScopes": m["userScopes"],
            "botScopes": m["botScopes"],
            "privacyUrl": m["privacyUrl"],
            "termsUrl": m["termsUrl"],
            "meetingSurface": m["meetingSurface"].is_object(),
            "publishedAt": at.to_rfc3339(),
        }),
        at,
        id,
    ))
}

/// `GET /v1/app-directory`.
pub async fn list(state: &AppState, page: &Page) -> Result<Value, ApiError> {
    if page.has_cursor() && page.cursor_key().is_none() {
        return Err(ApiError(ErrorCode::ValidationError));
    }
    let (at, id) = page.cursor_key().unzip();
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "{SELECT} WHERE {LISTED} \
                   AND ($1::timestamptz IS NULL OR (a.published_at, a.app_id) < ($1, $2)) \
                 ORDER BY a.published_at DESC, a.app_id DESC LIMIT $3"
            ),
            vec![at.into(), id.into(), (page.limit as i64 + 1).into()],
        ))
        .await?;
    let mut items = Vec::new();
    let mut last = None;
    for r in rows.iter().take(page.limit) {
        let (v, at, id) = entry(r)?;
        items.push(v);
        last = Some((at, id));
    }
    let next = (rows.len() > page.limit)
        .then(|| last.map(|(a, i)| Page::key_cursor(a, i)))
        .flatten();
    Ok(json!({ "items": items, "nextCursor": next }))
}

/// `GET /v1/app-directory/{appId}`.
pub async fn get(state: &AppState, app_id: Uuid) -> Result<Value, ApiError> {
    let row = state
        .db
        .query_one(stmt(
            &format!("{SELECT} WHERE {LISTED} AND a.app_id = $1"),
            vec![app_id.into()],
        ))
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    Ok(entry(&row)?.0)
}
