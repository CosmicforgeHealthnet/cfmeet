//! Notification Service roadmap N10 (Document 5 §6, Phase 4.2): notification preferences
//! move to the Notification Service, the single preference writer.
//!
//! **Copy + catch-up (Hub side).** Two owner APIs of contract
//! `communications/notification-owner/v1` (Notification Service only,
//! `communications-preferences.export`, first-party delegated scope validated with
//! Platform Access, as for X4). The service pulls; the Hub never writes another
//! service's database and the service never reads the Hub's:
//! * `POST …/notification-preferences/export` pages the Hub's rows of one kind in key
//!   order — `ACCOUNT` (`communication_notification_preferences`, the 7 switches),
//!   `CONVERSATION` (`notifications` / `muted_until` of
//!   `communication_conversation_preferences`) and `CONTACT`
//!   (`communication_contact_aliases.notifications`, keyed by the contact's account
//!   reference) — each with its Hub `version` (the service records it as
//!   `source_version`), plus the change-feed positions read BEFORE the rows.
//! * `POST …/notification-preferences/changes` returns the `NOTIFICATION_PREFERENCE`,
//!   `CONVERSATION_PREFERENCE` and `CONTACT` entries of the existing change feed after a
//!   position, each resolved to the row's CURRENT value (an entry says what changed; the
//!   value is always read now, so a re-read is harmless).
//!
//! Both answer 409 `PREFERENCE_AUTHORITY_MOVED` once the switch is `SERVICE`: from then
//! on the Hub rows are only a mirror and must never be copied over the service's values.
//!
//! **Switch.** `communication_preference_authority.mode` (default `HUB`, today's
//! behaviour), changed only by the release tooling (version-guarded):
//! * `HUB` — the Hub reads and writes its own rows (unchanged code paths);
//! * `FREEZE` — the < 60 s write freeze of the final copy pass: reads stay local,
//!   preference writes answer 503 `COMMUNICATIONS_UNAVAILABLE` + `Retry-After: 5`;
//! * `SERVICE` — `GET/PUT /me/notification-preferences`, the `notifications` /
//!   `mutedUntil` of `GET/PUT /conversations/{id}/preferences` and the `notifications`
//!   field of `PATCH /contacts/{id}` read / write through the service (I2
//!   `notifications-user.forward`, U6 / U7). Client contracts are unchanged. A write is
//!   committed in the Hub only after the service accepted it, in the same transaction
//!   (the Hub row becomes a mirror of the accepted value); if the service is
//!   unavailable the request fails (503) and nothing is written locally.
//!
//! The write transaction share-locks the switch row, so a switch change waits for
//! in-flight preference writes and every later write sees the new mode.
use crate::{
    auth,
    error::{Error, Result},
    model::Profile,
    store, State,
};
use axum::{
    extract::State as HttpState,
    http::{HeaderValue, Method},
    response::IntoResponse,
    Extension, Json,
};
use cf_communications_policy::Scope;
use cf_service_auth::{
    DelegatedPlatformContext, InternalServiceContext, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// I2 `dispatchNotificationsUser` of the Notification Service.
pub const DISPATCH_PATH: &str = "/internal/v1/notifications/dispatch";
/// Rows per export / change page (contract `maximum`).
pub const MAX_PAGE: i64 = 500;
/// Attempts of an entry write (conversation / contact) against the service document.
pub const ENTRY_ATTEMPTS: usize = 3;
/// Change-feed entries younger than this are not "settled": a transaction that took its
/// sequence number earlier may still commit, so the service re-reads them.
pub const SETTLE_SECONDS: i64 = 60;

/// The preference-authority switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    Hub,
    Freeze,
    Service,
}

impl Authority {
    fn parse(mode: &str) -> Self {
        match mode {
            "HUB" => Self::Hub,
            "SERVICE" => Self::Service,
            // FREEZE, and anything unexpected: no preference write is accepted.
            _ => Self::Freeze,
        }
    }
}

/// The three preference kinds the Hub exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExportKind {
    Account,
    Conversation,
    Contact,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportQuery {
    pub kind: ExportKind,
    #[serde(default)]
    pub after: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangesQuery {
    pub after: i64,
    pub limit: i64,
}

fn moved() -> Error {
    Error::Owner {
        status: 409,
        code: "PREFERENCE_AUTHORITY_MOVED",
    }
}

/// The client view of `GET/PUT /me/notification-preferences` from the service document
/// (U6 / U7): the same 7 switches, the service version, security events fixed on.
pub fn account_view_from_service(doc: &Value) -> Result<Value> {
    let c = &doc["categories"];
    let get = |k: &str| c[k].as_bool().ok_or(Error::Unavailable);
    Ok(
        json!({"schemaVersion":1,"version":doc["version"].as_i64().ok_or(Error::Unavailable)?,
        "directMessages":get("directMessages")?,"groupMessages":get("groupMessages")?,"incomingCalls":get("incomingCalls")?,
        "missedCalls":get("missedCalls")?,"contactRequests":get("contactRequests")?,"groupInvitations":get("groupInvitations")?,
        "meetingInvitations":get("meetingInvitations")?,"securityEvents":true,
        "payloadPolicy":"ROUTING_METADATA_ONLY"}),
    )
}

impl State {
    /// The current switch (`HUB` when the row is absent). With `lock` (inside a write
    /// transaction) the row is share-locked until the transaction ends.
    pub async fn preference_authority<C: ConnectionTrait>(
        &self,
        db: &C,
        lock: bool,
    ) -> Result<Authority> {
        let v: Option<Value> = store::one(
            db,
            &format!(
                "SELECT jsonb_build_object('mode',mode) doc FROM communication_preference_authority WHERE singleton{}",
                if lock { " FOR SHARE" } else { "" }
            ),
            vec![],
        )
        .await?;
        Ok(v.and_then(|v| v["mode"].as_str().map(Authority::parse))
            .unwrap_or(Authority::Hub))
    }

    /// One I2 call for the profile's account (`/preferences` only).
    async fn notifications_dispatch(
        &self,
        p: &Profile,
        method: &str,
        body: Option<Value>,
        key: Option<Uuid>,
    ) -> Result<(u16, Value)> {
        if !self.config.notifications_configured {
            return Err(Error::DependencyNotConfigured);
        }
        let mut d = json!({"method":method,"path":"/preferences","account":p.account_reference});
        if let Some(k) = key {
            d["idempotencyKey"] = json!(k.to_string());
        }
        if let Some(b) = body {
            d["body"] = b;
        }
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        let response = self
            .config
            .client
            .exchange_json(&request, Method::POST, DISPATCH_PATH, Some(&d))
            .await
            .map_err(|_| Error::Unavailable)?;
        let status = response.status();
        let value: Value = response.json().map_err(|_| Error::Unavailable)?;
        if status >= 500 {
            tracing::warn!(
                service = "communications",
                status,
                "notification preference facade: service unavailable"
            );
        }
        Ok((status, value))
    }

    /// U6: the service's preference document for this profile.
    pub async fn service_preferences(&self, p: &Profile) -> Result<Value> {
        let (status, v) = self.notifications_dispatch(p, "GET", None, None).await?;
        match status {
            200 if v["schemaVersion"] == 1
                && v["version"].is_i64()
                && v["categories"].is_object() =>
            {
                Ok(v)
            }
            429 => Err(Error::RateLimited),
            _ => Err(Error::Unavailable),
        }
    }

    /// U7; `Ok(None)` is a version conflict.
    pub async fn service_update(
        &self,
        p: &Profile,
        update: Value,
        key: Uuid,
    ) -> Result<Option<Value>> {
        let (status, v) = self
            .notifications_dispatch(p, "PUT", Some(update), Some(key))
            .await?;
        match status {
            200 if v["schemaVersion"] == 1
                && v["version"].is_i64()
                && v["categories"].is_object() =>
            {
                Ok(Some(v))
            }
            409 if v["error"]["code"] == "VERSION_CONFLICT" => Ok(None),
            400 => Err(Error::Validation),
            429 => Err(Error::RateLimited),
            _ => Err(Error::Unavailable),
        }
    }

    /// U7 of one conversation or contact entry. The client's optimistic version is the
    /// Hub row's (already checked under its row lock); the service document version is
    /// read just before the write and the write is retried on a concurrent change.
    pub async fn service_update_entry(&self, p: &Profile, partial: Value, key: Uuid) -> Result<()> {
        for _ in 0..ENTRY_ATTEMPTS {
            let doc = self.service_preferences(p).await?;
            let mut body = partial.clone();
            body["expectedVersion"] = doc["version"].clone();
            if self.service_update(p, body, key).await?.is_some() {
                return Ok(());
            }
        }
        Err(Error::Unavailable)
    }

    /// The Hub conversation row with `notifications` / `muted_until` taken from the
    /// service document (absent there: `ALL`, no mute). `mutedUntil` is formatted by
    /// PostgreSQL exactly as the Hub row would be, so responses are identical.
    pub async fn service_conversation_row(
        &self,
        p: &Profile,
        conversation: Uuid,
        local: Option<Value>,
    ) -> Result<Value> {
        let doc = self.service_preferences(p).await?;
        let entry = &doc["conversations"][conversation.to_string()];
        let notifications = entry["notifications"].as_str().unwrap_or("ALL").to_owned();
        if !matches!(notifications.as_str(), "ALL" | "MENTIONS" | "MUTED") {
            return Err(Error::Unavailable);
        }
        let muted_until = match entry["mutedUntil"].as_str() {
            None => Value::Null,
            Some(t) => {
                let t: DateTime<Utc> = DateTime::parse_from_rfc3339(t)
                    .map_err(|_| Error::Unavailable)?
                    .with_timezone(&Utc);
                let v: Option<Value> = store::one(
                    &self.store.db,
                    "SELECT jsonb_build_object('m',$1::timestamptz) doc",
                    vec![t.into()],
                )
                .await?;
                v.map(|v| v["m"].clone()).unwrap_or(Value::Null)
            }
        };
        let mut row = local.unwrap_or_else(|| json!({"media_auto_download":"WIFI","version":0}));
        row["notifications"] = json!(notifications);
        row["muted_until"] = muted_until;
        Ok(row)
    }

    /// Feed positions: (latest, settled, oldest retained).
    async fn feed_positions(&self) -> Result<(i64, i64, i64)> {
        let v: Value = store::one(&self.store.db,
            &format!("SELECT jsonb_build_object(
                'latest',COALESCE((SELECT seq FROM communication_changes ORDER BY seq DESC LIMIT 1),0),
                'settled',COALESCE((SELECT seq FROM communication_changes WHERE created_at<now()-interval '{SETTLE_SECONDS} seconds' ORDER BY created_at DESC,seq DESC LIMIT 1),0),
                'oldest',COALESCE((SELECT seq FROM communication_changes ORDER BY seq LIMIT 1),0)) doc"),
            vec![]).await?.ok_or(Error::Unavailable)?;
        Ok((
            v["latest"].as_i64().unwrap_or(0),
            v["settled"].as_i64().unwrap_or(0),
            v["oldest"].as_i64().unwrap_or(0),
        ))
    }

    /// One export page (Notification Service copy).
    pub async fn export_notification_preferences(
        &self,
        scope: &DelegatedPlatformContext,
        q: &ExportQuery,
    ) -> Result<Value> {
        if !(1..=MAX_PAGE).contains(&q.limit) {
            return Err(Error::Validation);
        }
        let uuid = |s: &str| Uuid::parse_str(s).map_err(|_| Error::Validation);
        let after: Option<(Uuid, Option<Uuid>)> = match (&q.after, q.kind) {
            (None, _) => None,
            (Some(a), ExportKind::Conversation) => {
                let (p, c) = a.split_once(':').ok_or(Error::Validation)?;
                Some((uuid(p)?, Some(uuid(c)?)))
            }
            (Some(a), _) => Some((uuid(a)?, None)),
        };
        auth::validate_scope(self, scope).await?;
        if self.preference_authority(&self.store.db, false).await? == Authority::Service {
            return Err(moved());
        }
        // Positions first: every change committed after them is in the feed after them.
        let (latest, settled, oldest) = self.feed_positions().await?;
        let mut v = store::scoped(Scope::of(scope));
        v.push(after.map(|a| a.0).into());
        v.push(after.and_then(|a| a.1).into());
        v.push((q.limit + 1).into());
        let sql = match q.kind {
            ExportKind::Account => "SELECT jsonb_build_object('cursor',p.id::text,'accountRef',p.account_reference,'version',n.version,
                    'directMessages',n.direct_messages,'groupMessages',n.group_messages,'incomingCalls',n.incoming_calls,
                    'missedCalls',n.missed_calls,'contactRequests',n.contact_requests,'groupInvitations',n.group_invitations,
                    'meetingInvitations',n.meeting_invitations) doc
                  FROM communication_notification_preferences n JOIN communication_profiles p ON p.id=n.profile_id
                 WHERE p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3 AND ($4::uuid IS NULL OR p.id>$4)
                   AND $5::uuid IS NULL
                 ORDER BY p.id LIMIT $6",
            ExportKind::Conversation => "SELECT jsonb_build_object('cursor',c.owner_profile_id::text||':'||c.conversation_id::text,
                    'accountRef',p.account_reference,'conversationId',c.conversation_id,'notifications',c.notifications,
                    'mutedUntil',c.muted_until,'version',c.version) doc
                  FROM communication_conversation_preferences c JOIN communication_profiles p ON p.id=c.owner_profile_id
                 WHERE p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3
                   AND ($4::uuid IS NULL OR (c.owner_profile_id,c.conversation_id)>($4::uuid,$5::uuid))
                 ORDER BY c.owner_profile_id,c.conversation_id LIMIT $6",
            ExportKind::Contact => "SELECT jsonb_build_object('cursor',a.id::text,'accountRef',p.account_reference,
                    'contactRef',o.account_reference,'notifications',a.notifications,'version',a.version) doc
                  FROM communication_contact_aliases a
                  JOIN communication_profiles p ON p.id=a.owner_profile_id
                  JOIN communication_connections k ON k.id=a.connection_id
                  JOIN communication_profiles o ON o.id=CASE WHEN k.left_profile_id=a.owner_profile_id THEN k.right_profile_id ELSE k.left_profile_id END
                 WHERE p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3 AND ($4::uuid IS NULL OR a.id>$4)
                   AND $5::uuid IS NULL
                 ORDER BY a.id LIMIT $6",
        };
        let mut rows: Vec<Value> = store::rows(&self.store.db, sql, v).await?;
        let more = rows.len() as i64 > q.limit;
        rows.truncate(q.limit as usize);
        let next = if more {
            rows.last()
                .and_then(|r| r["cursor"].as_str().map(str::to_owned))
        } else {
            None
        };
        for r in rows.iter_mut() {
            if let Some(o) = r.as_object_mut() {
                o.remove("cursor");
            }
        }
        Ok(
            json!({"schemaVersion":1,"kind":q.kind_name(),"items":rows,"nextCursor":next,
            "feedPosition":latest,"settledPosition":settled,"oldestPosition":oldest}),
        )
    }

    /// Change-feed entries of the three preference domains after `after`, resolved to the
    /// current values (Notification Service catch-up).
    pub async fn notification_preference_changes(
        &self,
        scope: &DelegatedPlatformContext,
        q: &ChangesQuery,
    ) -> Result<Value> {
        if !(1..=MAX_PAGE).contains(&q.limit) || q.after < 0 {
            return Err(Error::Validation);
        }
        auth::validate_scope(self, scope).await?;
        if self.preference_authority(&self.store.db, false).await? == Authority::Service {
            return Err(moved());
        }
        let (latest, settled, oldest) = self.feed_positions().await?;
        let mut v = store::scoped(Scope::of(scope));
        v.push(q.after.into());
        v.push((q.limit + 1).into());
        let mut rows: Vec<Value> = store::rows(&self.store.db,
            "SELECT jsonb_build_object('seq',f.seq,'domain',f.domain,'accountRef',p.account_reference,'resourceId',f.resource_id,
                'account',(SELECT jsonb_build_object('version',n.version,'directMessages',n.direct_messages,'groupMessages',n.group_messages,
                    'incomingCalls',n.incoming_calls,'missedCalls',n.missed_calls,'contactRequests',n.contact_requests,
                    'groupInvitations',n.group_invitations,'meetingInvitations',n.meeting_invitations)
                  FROM communication_notification_preferences n WHERE f.domain='NOTIFICATION_PREFERENCE' AND n.profile_id=f.profile_id),
                'conversation',(SELECT jsonb_build_object('notifications',c.notifications,'mutedUntil',c.muted_until,'version',c.version)
                  FROM communication_conversation_preferences c
                 WHERE f.domain='CONVERSATION_PREFERENCE' AND c.owner_profile_id=f.profile_id AND c.conversation_id=f.resource_id),
                'contact',(SELECT jsonb_build_object('contactRef',o.account_reference,'notifications',a.notifications,'version',a.version)
                  FROM communication_contact_aliases a
                  JOIN communication_connections k ON k.id=a.connection_id
                  JOIN communication_profiles o ON o.id=CASE WHEN k.left_profile_id=a.owner_profile_id THEN k.right_profile_id ELSE k.left_profile_id END
                 WHERE f.domain='CONTACT' AND a.owner_profile_id=f.profile_id AND (a.id=f.resource_id OR a.connection_id=f.resource_id)
                 ORDER BY a.id LIMIT 1)) doc
               FROM communication_changes f JOIN communication_profiles p ON p.id=f.profile_id
              WHERE f.seq>$4 AND f.domain IN ('NOTIFICATION_PREFERENCE','CONVERSATION_PREFERENCE','CONTACT')
                AND p.tenant_id=$1 AND p.application_id=$2 AND p.environment_id=$3
              ORDER BY f.seq LIMIT $5",
            v).await?;
        let more = rows.len() as i64 > q.limit;
        rows.truncate(q.limit as usize);
        let next = rows
            .last()
            .and_then(|r| r["seq"].as_i64())
            .unwrap_or(q.after);
        let items: Vec<Value> = rows
            .iter()
            .filter_map(|r| {
                let (seq, account) = (&r["seq"], &r["accountRef"]);
                match r["domain"].as_str()? {
                    "NOTIFICATION_PREFERENCE" => Some(
                        json!({"seq":seq,"kind":"ACCOUNT","accountRef":account,"preference":r["account"]}),
                    ),
                    "CONVERSATION_PREFERENCE" => r["resourceId"].as_str().map(|c| {
                        json!({"seq":seq,"kind":"CONVERSATION","accountRef":account,"conversationId":c,"preference":r["conversation"]})
                    }),
                    // A CONTACT entry that names no alias of this owner (a request, a list)
                    // carries no preference.
                    "CONTACT" => r["contact"].as_object().map(|c| {
                        json!({"seq":seq,"kind":"CONTACT","accountRef":account,"contactRef":c.get("contactRef"),
                            "preference":{"notifications":c.get("notifications"),"version":c.get("version")}})
                    }),
                    _ => None,
                }
            })
            .collect();
        Ok(
            json!({"schemaVersion":1,"items":items,"nextAfter":next,"more":more,
            "feedPosition":latest,"settledPosition":settled,"oldestPosition":oldest}),
        )
    }
}

impl ExportQuery {
    fn kind_name(&self) -> &'static str {
        match self.kind {
            ExportKind::Account => "ACCOUNT",
            ExportKind::Conversation => "CONVERSATION",
            ExportKind::Contact => "CONTACT",
        }
    }
}

fn no_store(v: Value) -> axum::response::Response {
    let mut response = Json(v).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

/// `POST /internal/v1/communications/notification-preferences/export` (Notification
/// Service only, `communications-preferences.export`).
pub async fn export(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(q): Json<ExportQuery>,
) -> Result<axum::response::Response> {
    if ctx.authenticated_service() != &ServiceId::notifications() {
        return Err(Error::Forbidden);
    }
    let scope = auth::delegated(&ctx)?;
    Ok(no_store(
        s.export_notification_preferences(&scope, &q).await?,
    ))
}

/// `POST /internal/v1/communications/notification-preferences/changes` (Notification
/// Service only, `communications-preferences.export`).
pub async fn changes(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(q): Json<ChangesQuery>,
) -> Result<axum::response::Response> {
    if ctx.authenticated_service() != &ServiceId::notifications() {
        return Err(Error::Forbidden);
    }
    let scope = auth::delegated(&ctx)?;
    Ok(no_store(
        s.notification_preference_changes(&scope, &q).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_modes_accept_no_write() {
        assert_eq!(Authority::parse("HUB"), Authority::Hub);
        assert_eq!(Authority::parse("SERVICE"), Authority::Service);
        assert_eq!(Authority::parse("FREEZE"), Authority::Freeze);
        assert_eq!(Authority::parse("hub"), Authority::Freeze);
    }

    #[test]
    fn the_service_document_maps_to_the_seven_switches() {
        let doc = json!({"schemaVersion":1,"version":7,"categories":{"directMessages":false,"groupMessages":true,
            "incomingCalls":true,"missedCalls":false,"contactRequests":true,"groupInvitations":true,
            "meetingInvitations":false,"meetingReminders":false,"voicemail":true},"securityEvents":true});
        let v = account_view_from_service(&doc).unwrap();
        assert_eq!(v["version"], 7);
        assert_eq!(v["directMessages"], false);
        assert_eq!(v["meetingInvitations"], false);
        assert_eq!(v["securityEvents"], true);
        assert_eq!(v["payloadPolicy"], "ROUTING_METADATA_ONLY");
        assert!(v.get("meetingReminders").is_none());
        assert_eq!(v.as_object().unwrap().len(), 11);
        let mut broken = doc.clone();
        broken["categories"]["groupMessages"] = Value::Null;
        assert!(account_view_from_service(&broken).is_err());
    }
}
