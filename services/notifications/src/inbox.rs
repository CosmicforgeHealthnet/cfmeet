//! The in-app inbox (roadmap N8; U1–U5, table 5). Items are written by W1 for routed
//! IN_APP deliveries; titles and bodies are rendered at read time from the stored template
//! and params (never codes, tokens, addresses, E2EE text or URLs). Owners see only their
//! own items: every query filters on the verified `recipient_ref`, and a foreign id is a
//! plain 404.
//!
//! Grouping: an unread item with the same `group_key` absorbs the next one
//! (`group_count + 1`, moved to the top). Silent items (category off, muted) are listed but
//! never counted as unread. Retention: `expires_at` (90 days by default, D-8).

use base64::Engine;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{db, templates};

pub struct NewItem<'a> {
    pub id: Uuid,
    pub recipient_ref: &'a str,
    pub category: &'a str,
    pub template: &'a str,
    pub params: &'a Value,
    pub action_kind: &'a str,
    pub action_ref: Option<Uuid>,
    pub group_key: Option<&'a str>,
    pub silent: bool,
    pub expires_at: DateTime<Utc>,
}

/// Writes (or groups) one item. Idempotent on the notification id.
pub async fn write<C: ConnectionTrait>(conn: &C, item: NewItem<'_>) -> Result<Uuid, DbErr> {
    if let Some(group) = item.group_key {
        let grouped = conn
            .query_one(db::stmt(
                "UPDATE notif_inbox_items
                    SET group_count = group_count + 1, created_at = now(), updated_at = now(), version = version + 1,
                        params = $3, silent = silent AND $4
                  WHERE recipient_ref = $1 AND group_key = $2 AND read_at IS NULL AND dismissed_at IS NULL AND id <> $5
              RETURNING id",
                vec![item.recipient_ref.into(), group.into(), item.params.clone().into(), item.silent.into(), item.id.into()],
            ))
            .await?;
        if let Some(r) = grouped {
            return r.try_get("", "id");
        }
    }
    db::exec(
        conn,
        "INSERT INTO notif_inbox_items (id, recipient_ref, category, template_key, params, action_kind, action_ref,
             group_key, silent, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT DO NOTHING",
        vec![
            item.id.into(),
            item.recipient_ref.into(),
            item.category.into(),
            item.template.into(),
            item.params.clone().into(),
            item.action_kind.into(),
            item.action_ref.into(),
            item.group_key.map(str::to_string).into(),
            item.silent.into(),
            item.expires_at.into(),
        ],
    )
    .await?;
    Ok(item.id)
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct ItemRow {
    pub id: Uuid,
    pub category: String,
    pub template_key: String,
    pub params: Value,
    pub action_kind: String,
    pub action_ref: Option<Uuid>,
    pub group_count: i32,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const ITEM_COLUMNS: &str = "id, category, template_key, params, action_kind, action_ref, group_count, created_at, read_at, dismissed_at, version";

/// The contract `NotificationItem`.
pub fn item_json(r: &ItemRow) -> Value {
    let (title, body) = templates::inbox_text(&r.template_key, &r.params);
    json!({
        "id": r.id,
        "category": r.category,
        "title": title,
        "body": body,
        "action": { "kind": r.action_kind, "ref": r.action_ref },
        "createdAt": r.created_at,
        "readAt": r.read_at,
        "dismissedAt": r.dismissed_at,
        "groupCount": r.group_count,
        "version": r.version,
    })
}

fn encode_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{}|{id}", at.timestamp_micros()))
}

pub fn decode_cursor(c: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (micros, id) = s.split_once('|')?;
    Some((
        DateTime::from_timestamp_micros(micros.parse().ok()?)?,
        Uuid::parse_str(id).ok()?,
    ))
}

pub async fn unread_count<C: ConnectionTrait>(conn: &C, recipient: &str) -> Result<i64, DbErr> {
    db::scalar_i64(
        conn,
        "SELECT count(*)::bigint AS n FROM notif_inbox_items
          WHERE recipient_ref = $1 AND read_at IS NULL AND dismissed_at IS NULL AND NOT silent AND expires_at > now()",
        vec![recipient.into()],
    )
    .await
}

pub struct ListQuery {
    pub cursor: Option<(DateTime<Utc>, Uuid)>,
    pub limit: i64,
    pub unread_only: bool,
    pub categories: Vec<String>,
}

/// U1.
pub async fn list<C: ConnectionTrait>(
    conn: &C,
    recipient: &str,
    q: &ListQuery,
) -> Result<Value, DbErr> {
    let (cursor_at, cursor_id) = match q.cursor {
        Some((a, i)) => (Some(a), Some(i)),
        None => (None, None),
    };
    let rows: Vec<ItemRow> = db::all(
        conn,
        &format!(
            "SELECT {ITEM_COLUMNS} FROM notif_inbox_items
              WHERE recipient_ref = $1 AND dismissed_at IS NULL AND expires_at > now()
                AND (NOT $2 OR read_at IS NULL)
                AND (cardinality($3::text[]) = 0 OR category = ANY($3::text[]))
                AND ($4::timestamptz IS NULL OR (created_at, id) < ($4::timestamptz, $5::uuid))
              ORDER BY created_at DESC, id DESC
              LIMIT $6"
        ),
        vec![
            recipient.into(),
            q.unread_only.into(),
            q.categories.clone().into(),
            cursor_at.into(),
            cursor_id.into(),
            (q.limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > q.limit;
    let page: Vec<&ItemRow> = rows.iter().take(q.limit as usize).collect();
    let next = if more {
        page.last().map(|r| encode_cursor(r.created_at, r.id))
    } else {
        None
    };
    Ok(json!({
        "items": page.iter().map(|r| item_json(r)).collect::<Vec<_>>(),
        "nextCursor": next,
        "unreadCount": unread_count(conn, recipient).await?,
    }))
}

/// U2 (with the ETag version: the latest change to the recipient's items).
pub async fn summary<C: ConnectionTrait>(
    conn: &C,
    recipient: &str,
) -> Result<(i64, Option<DateTime<Utc>>, i64), DbErr> {
    let row = conn
        .query_one(db::stmt(
            "SELECT count(*) FILTER (WHERE read_at IS NULL AND dismissed_at IS NULL AND NOT silent)::bigint AS unread,
                    max(created_at) FILTER (WHERE dismissed_at IS NULL) AS latest,
                    COALESCE((EXTRACT(EPOCH FROM max(updated_at)) * 1000)::bigint, 0) + count(*)::bigint AS version
               FROM notif_inbox_items WHERE recipient_ref = $1 AND expires_at > now()",
            vec![recipient.into()],
        ))
        .await?;
    let r = row.ok_or_else(|| DbErr::Custom("summary returned no row".into()))?;
    Ok((
        r.try_get("", "unread")?,
        r.try_get("", "latest")?,
        r.try_get("", "version")?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUpdate {
    #[serde(default)]
    pub read: Option<bool>,
    #[serde(default)]
    pub dismissed: Option<bool>,
    pub expected_version: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    Updated(Box<Value>),
    NotFound,
    VersionConflict,
}

/// U3: own items only (else not found).
pub async fn update<C: ConnectionTrait>(
    conn: &C,
    recipient: &str,
    id: Uuid,
    u: &ItemUpdate,
) -> Result<UpdateOutcome, DbErr> {
    let row: Option<ItemRow> = db::one(
        conn,
        &format!(
            "UPDATE notif_inbox_items
                SET read_at = CASE WHEN $3::boolean IS NULL THEN read_at WHEN $3 THEN COALESCE(read_at, now()) ELSE NULL END,
                    dismissed_at = CASE WHEN $4::boolean IS NULL THEN dismissed_at WHEN $4 THEN COALESCE(dismissed_at, now()) ELSE NULL END,
                    version = version + 1, updated_at = now()
              WHERE id = $1 AND recipient_ref = $2 AND version = $5 AND expires_at > now()
          RETURNING {ITEM_COLUMNS}"
        ),
        vec![id.into(), recipient.into(), u.read.into(), u.dismissed.into(), u.expected_version.into()],
    )
    .await?;
    if let Some(r) = row {
        return Ok(UpdateOutcome::Updated(Box::new(item_json(&r))));
    }
    let exists = conn
        .query_one(db::stmt(
            "SELECT 1 AS x FROM notif_inbox_items WHERE id = $1 AND recipient_ref = $2 AND expires_at > now()",
            vec![id.into(), recipient.into()],
        ))
        .await?
        .is_some();
    Ok(if exists {
        UpdateOutcome::VersionConflict
    } else {
        UpdateOutcome::NotFound
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BulkRequest {
    #[serde(default)]
    pub ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub up_to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub category: Option<Vec<String>>,
}

impl BulkRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        match (&self.ids, &self.up_to) {
            (Some(ids), None) if !ids.is_empty() && ids.len() <= 100 => Ok(()),
            (None, Some(_)) => Ok(()),
            (Some(_), None) => Err("ids holds 1 to 100 identifiers"),
            _ => Err("exactly one of ids or upTo is required"),
        }
    }
}

/// U4 (`read`) and U5 (`dismiss`): returns the number of changed items.
pub async fn bulk<C: ConnectionTrait>(
    conn: &C,
    recipient: &str,
    req: &BulkRequest,
    dismiss: bool,
) -> Result<u64, DbErr> {
    let set = if dismiss {
        "dismissed_at = COALESCE(dismissed_at, now())"
    } else {
        "read_at = COALESCE(read_at, now())"
    };
    let pending = if dismiss {
        "dismissed_at IS NULL"
    } else {
        "read_at IS NULL AND dismissed_at IS NULL"
    };
    db::exec(
        conn,
        &format!(
            "UPDATE notif_inbox_items SET {set}, version = version + 1, updated_at = now()
              WHERE recipient_ref = $1 AND {pending} AND expires_at > now()
                AND ($2::uuid[] IS NULL OR id = ANY($2::uuid[]))
                AND ($3::timestamptz IS NULL OR created_at <= $3)
                AND ($4::text[] IS NULL OR category = ANY($4::text[]))"
        ),
        vec![
            recipient.into(),
            req.ids.clone().into(),
            req.up_to.into(),
            req.category.clone().into(),
        ],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let at = Utc::now();
        let id = Uuid::new_v4();
        let c = encode_cursor(at, id);
        let (a, i) = decode_cursor(&c).unwrap();
        assert_eq!(i, id);
        assert_eq!(a.timestamp_micros(), at.timestamp_micros());
        assert!(decode_cursor("not-a-cursor").is_none());
    }

    #[test]
    fn bulk_requests_need_exactly_one_selector() {
        let ok = BulkRequest {
            ids: Some(vec![Uuid::new_v4()]),
            up_to: None,
            category: None,
        };
        assert!(ok.validate().is_ok());
        let both = BulkRequest {
            ids: Some(vec![Uuid::new_v4()]),
            up_to: Some(Utc::now()),
            category: None,
        };
        assert!(both.validate().is_err());
        let too_many = BulkRequest {
            ids: Some((0..101).map(|_| Uuid::new_v4()).collect()),
            up_to: None,
            category: None,
        };
        assert!(too_many.validate().is_err());
    }
}
