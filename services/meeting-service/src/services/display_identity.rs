//! Safe display identity of accounts (ORG-006, Blueprint Section 17).
//!
//! Organization and team member lists carry only ids from the Organizations Service. The
//! Meeting Service owns the accounts, so it joins the display identity here: the
//! presentation name (chosen display name, otherwise the username) and the username.
//! Never an email address, a status or any other account attribute.
//!
//! Enrichment is best effort: a database failure leaves the names `null` (logged) and
//! never fails a request whose authoritative answer already succeeded (for example a
//! relayed write).

use std::collections::{BTreeSet, HashMap};

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::models::users;

/// At most this many ids per database query.
const CHUNK: usize = 500;

/// The display identity of one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayIdentity {
    pub display_name: String,
    pub username: String,
}

/// The display identity of every known account in `ids` (unknown ids are absent).
pub async fn lookup<C: ConnectionTrait>(
    conn: &C,
    ids: impl IntoIterator<Item = Uuid>,
) -> HashMap<Uuid, DisplayIdentity> {
    let ids: Vec<Uuid> = ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut out = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(CHUNK) {
        match users::Entity::find()
            .filter(users::Column::Id.is_in(chunk.to_vec()))
            .all(conn)
            .await
        {
            Ok(rows) => {
                for u in rows {
                    let display_name = u.presentation_name();
                    out.insert(
                        u.id,
                        DisplayIdentity {
                            display_name,
                            username: u.username,
                        },
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "display identity lookup failed; names omitted");
                return out;
            }
        }
    }
    out
}

fn user_id(item: &Value) -> Option<Uuid> {
    item.get("userId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Adds `displayName` and `username` (or `null`) to a relayed member document, or to
/// every item of a `{items: [...]}` page, keyed by its `userId`.
pub async fn enrich_members<C: ConnectionTrait>(conn: &C, body: &mut Value) {
    let ids: Vec<Uuid> = match body.get("items").and_then(Value::as_array) {
        Some(items) => items.iter().filter_map(user_id).collect(),
        None => user_id(body).into_iter().collect(),
    };
    let known = lookup(conn, ids).await;
    let apply = |item: &mut Value| {
        let identity = user_id(item).and_then(|id| known.get(&id));
        if let Some(o) = item.as_object_mut() {
            o.insert(
                "displayName".into(),
                json!(identity.map(|i| i.display_name.as_str())),
            );
            o.insert(
                "username".into(),
                json!(identity.map(|i| i.username.as_str())),
            );
        }
    };
    match body.get_mut("items").and_then(Value::as_array_mut) {
        Some(items) => items.iter_mut().for_each(apply),
        None => apply(body),
    }
}

/// Adds `createdByDisplayName` to a relayed organization document when its `createdBy`
/// account is known.
pub async fn enrich_creator<C: ConnectionTrait>(conn: &C, body: &mut Value) {
    let Some(creator) = body
        .get("createdBy")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return;
    };
    if let Some(identity) = lookup(conn, [creator]).await.remove(&creator) {
        if let Some(o) = body.as_object_mut() {
            o.insert(
                "createdByDisplayName".into(),
                Value::String(identity.display_name),
            );
        }
    }
}
