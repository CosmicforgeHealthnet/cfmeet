//! S2 — account deletion and data download: the Hub's part (Meeting Service only,
//! `communications-account.apply`, the same grant as the account lifecycle events).
//!
//! * `POST /internal/v1/communications/account-erasure` `{accountReference}` — in one
//!   transaction the account's Hub data goes: its CF Meet IDs (current and retired, so none
//!   resolves again), its contacts (names it gave, favourites), contact lists, saved
//!   messages, per-conversation preferences and clear-for-me marks, its call / meeting
//!   timeline entries and its change feed; its open invites are revoked, pending contact
//!   requests to or from it are cancelled and its connections end (REVOKED; a BLOCKED
//!   relationship stays blocked for the other person's safety). The profile row itself is
//!   kept only as the anchor other people's records reference: its display name becomes a
//!   neutral placeholder, its avatar is removed and it is DEACTIVATED with credentials
//!   revoked from now, which fans out to Calling and Messaging through the durable outbox.
//!   The append-only audit records `ACCOUNT_ERASED` with counts only. Idempotent: a repeat
//!   finds nothing left and answers zero counts; an account that never opened the Hub
//!   answers `profile: false`.
//! * `POST /internal/v1/communications/account-export` `{accountReference}` — the same data
//!   read back for the account's download (bounded lists, the account's own entries only).
use crate::{
    auth,
    error::{Error, Result},
    model::*,
    store::{self, execute, one},
    State,
};
use axum::{extract::State as HttpState, Extension, Json};
use cf_communications_policy::Scope;
use cf_service_auth::InternalServiceContext;
use serde::Deserialize;
use serde_json::{json, Value};

/// The display name an erased profile keeps (other people's records still point at it).
pub const ERASED_DISPLAY_NAME: &str = "Deleted account";
/// At most this many rows per list in an export.
const EXPORT_LIMIT: i64 = 1000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountRequest {
    pub account_reference: String,
}

impl AccountRequest {
    fn valid(&self) -> bool {
        self.account_reference
            .strip_prefix("cfmeet-user:")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
    }
}

const LOOKUP: &str = "SELECT to_jsonb(p) doc FROM communication_profiles p WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND account_reference=$4";

async fn scope_of(s: &State, ctx: &InternalServiceContext) -> Result<Scope> {
    let scope = auth::delegated(ctx)?;
    auth::validate_scope(s, &scope).await?;
    Ok(Scope::of(&scope))
}

pub async fn erase(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(request): Json<AccountRequest>,
) -> Result<Json<Value>> {
    if !request.valid() {
        return Err(Error::Validation);
    }
    let scope = scope_of(&s, &ctx).await?;
    let mut values = store::scoped(scope);
    values.push(request.account_reference.clone().into());
    let txn = s.store.begin().await?;
    let Some(p): Option<Profile> = one(&txn, &format!("{LOOKUP} FOR UPDATE"), values).await? else {
        txn.commit().await?;
        return Ok(Json(json!({"schemaVersion":1,"profile":false,"erased":{}})));
    };
    let id = p.id;
    let mut counts = serde_json::Map::new();
    for (name, sql) in [
        ("cfMeetIds", "DELETE FROM communication_public_ids WHERE profile_id=$1"),
        // List members go with their lists and with the contacts (ON DELETE CASCADE).
        ("contactLists", "DELETE FROM communication_contact_lists WHERE owner_profile_id=$1"),
        ("contacts", "DELETE FROM communication_contact_aliases WHERE owner_profile_id=$1"),
        ("savedMessages", "DELETE FROM communication_saved_messages WHERE owner_profile_id=$1"),
        ("conversationPreferences", "DELETE FROM communication_conversation_preferences WHERE owner_profile_id=$1"),
        ("clearForMeMarks", "DELETE FROM communication_conversation_watermarks WHERE owner_profile_id=$1"),
        ("timelineEntries", "DELETE FROM communication_timeline_entries WHERE viewer_profile_id=$1"),
        ("changeFeed", "DELETE FROM communication_changes WHERE profile_id=$1"),
        ("invitesRevoked", "UPDATE communication_invites SET revoked_at=now(),version=version+1 WHERE owner_profile_id=$1 AND revoked_at IS NULL"),
        ("contactRequestsCancelled", "UPDATE communication_contact_requests SET state='CANCELLED',version=version+1,updated_at=now() WHERE state='PENDING' AND (sender_profile_id=$1 OR recipient_profile_id=$1)"),
        ("connectionsEnded", "UPDATE communication_connections SET state='REVOKED',revoked_at=now(),version=version+1,updated_at=now() WHERE (left_profile_id=$1 OR right_profile_id=$1) AND state IN ('NONE','REQUEST_PENDING','CONNECTED')"),
    ] {
        counts.insert(name.into(), json!(execute(&txn, sql, vec![id.into()]).await?));
    }
    execute(
        &txn,
        "UPDATE communication_profiles SET display_name=$2,avatar_reference=NULL,activation_state='DEACTIVATED',deactivated_at=COALESCE(deactivated_at,now()),credentials_not_before=GREATEST(COALESCE(credentials_not_before,now()),now()),version=version+1,updated_at=now() WHERE id=$1",
        vec![id.into(), ERASED_DISPLAY_NAME.into()],
    )
    .await?;
    s.store
        .audit_system(
            &txn,
            p.scope(),
            "ACCOUNT_ERASED",
            Some(id),
            Value::Object(counts.clone()),
        )
        .await?;
    // Calling and Messaging end every credential of the principal (the same durable fan-out
    // as a deactivation).
    s.store
        .event(
            &txn,
            &p,
            "communications.account.changed",
            id,
            p.version + 1,
            json!({"principalId":p.principal_id,"state":"DEACTIVATED","credentialsNotBefore":chrono::Utc::now().timestamp()}),
        )
        .await?;
    txn.commit().await?;
    Ok(Json(
        json!({"schemaVersion":1,"profile":true,"erased":counts}),
    ))
}

pub async fn export(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(request): Json<AccountRequest>,
) -> Result<Json<Value>> {
    if !request.valid() {
        return Err(Error::Validation);
    }
    let scope = scope_of(&s, &ctx).await?;
    let mut values = store::scoped(scope);
    values.push(request.account_reference.clone().into());
    let db = &s.store.db;
    let Some(p): Option<Profile> = one(db, LOOKUP, values).await? else {
        return Ok(Json(json!({"schemaVersion":1,"profile":null})));
    };
    let list = |sql: &str| {
        format!("SELECT COALESCE(jsonb_agg(x),'[]'::jsonb) doc FROM ({sql} LIMIT {EXPORT_LIMIT}) x")
    };
    let id = p.id;
    let mut out = serde_json::Map::new();
    for (name, sql) in [
        ("cfMeetIds", "SELECT public_id AS \"cfMeetId\", current FROM communication_public_ids WHERE profile_id=$1 ORDER BY current DESC"),
        ("privacy", "SELECT to_jsonb(v) - 'profile_id' - 'version' AS settings FROM communication_privacy v WHERE profile_id=$1"),
        ("notificationPreferences", "SELECT to_jsonb(v) - 'profile_id' - 'version' AS settings FROM communication_notification_preferences v WHERE profile_id=$1"),
        ("contacts", "SELECT a.id AS \"contactId\", a.alias, a.favorite, c.state AS \"connectionState\", a.created_at AS \"createdAt\" FROM communication_contact_aliases a JOIN communication_connections c ON c.id=a.connection_id WHERE a.owner_profile_id=$1 ORDER BY a.created_at"),
        ("contactLists", "SELECT l.id AS \"listId\", l.name, (SELECT COALESCE(jsonb_agg(m.contact_id),'[]'::jsonb) FROM communication_contact_list_members m WHERE m.list_id=l.id) AS \"contactIds\" FROM communication_contact_lists l WHERE l.owner_profile_id=$1 ORDER BY l.position, l.id"),
        ("savedMessages", "SELECT conversation_id AS \"conversationId\", message_id AS \"messageId\", created_at AS \"savedAt\" FROM communication_saved_messages WHERE owner_profile_id=$1 ORDER BY created_at DESC"),
        ("conversationPreferences", "SELECT conversation_id AS \"conversationId\", notifications, muted_until AS \"mutedUntil\", media_auto_download AS \"mediaAutoDownload\" FROM communication_conversation_preferences WHERE owner_profile_id=$1 ORDER BY conversation_id"),
        ("devices", "SELECT platform, label, state, created_at AS \"createdAt\", revoked_at AS \"revokedAt\" FROM communication_devices WHERE profile_id=$1 ORDER BY created_at"),
        ("contactRequests", "SELECT id AS \"requestId\", CASE WHEN sender_profile_id=$1 THEN 'SENT' ELSE 'RECEIVED' END AS direction, state, created_at AS \"createdAt\" FROM communication_contact_requests WHERE sender_profile_id=$1 OR recipient_profile_id=$1 ORDER BY created_at DESC"),
    ] {
        let found: Option<Value> = one(db, &list(sql), vec![id.into()]).await?;
        out.insert(name.into(), found.unwrap_or_else(|| json!([])));
    }
    Ok(Json(json!({
        "schemaVersion": 1,
        "profile": {
            "displayName": p.display_name,
            "activationState": p.activation_state,
            "createdAt": p.created_at,
        },
        "data": out,
        "limitPerList": EXPORT_LIMIT,
    })))
}
