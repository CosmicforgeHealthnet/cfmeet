use crate::{
    auth, crypto,
    error::{Error, Result},
    model::*,
    relationships::uuid_field,
    store::{self, execute, one, rows},
    State,
};
use axum::{extract::State as HttpState, Extension, Json};
use cf_communications_policy::Scope;
use cf_service_auth::{
    DelegatedPlatformContext, InternalServiceContext, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountEvent {
    pub event_id: Uuid,
    pub account_reference: String,
    pub source_version: i64,
    pub kind: AccountEventKind,
    pub state: Activation,
    pub occurred_at: DateTime<Utc>,
    /// ACCOUNT_SESSION_ENDED only: the ended Account session (opaque reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_session_ref: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AccountEventKind {
    StatusChanged,
    SessionRevoked,
    /// Section 14 (ACC-008): one Account session ended (single sign-out, session revoke).
    /// Ends the Hub devices that session linked; the account epoch is unchanged.
    AccountSessionEnded,
}
impl AccountEvent {
    /// Credentials issued before `occurredAt` must stop working everywhere.
    pub fn revokes_credentials(&self) -> bool {
        self.kind == AccountEventKind::SessionRevoked
            || (self.kind == AccountEventKind::StatusChanged
                && matches!(self.state, Activation::Suspended | Activation::Deactivated))
    }
    /// Shape rules beyond the schema: a session reference only (and always) with
    /// ACCOUNT_SESSION_ENDED.
    pub fn well_formed(&self) -> bool {
        self.source_version >= 1
            && self.account_reference.len() <= 80
            && self.occurred_at <= Utc::now() + chrono::Duration::seconds(30)
            && match (self.kind, self.account_session_ref.as_deref()) {
                (AccountEventKind::AccountSessionEnded, Some(r)) => {
                    cf_communications_policy::valid_account_session_ref(r)
                }
                (AccountEventKind::AccountSessionEnded, None) => false,
                (_, r) => r.is_none(),
            }
    }
}

async fn inbox(
    s: &State,
    txn: &sea_orm::DatabaseTransaction,
    source: &str,
    event: Uuid,
    body: &Value,
) -> Result<bool> {
    let hash = crypto::digest(body);
    let inserted=execute(txn,"INSERT INTO communication_inbox_dedupe(source,event_id,request_digest) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",vec![source.into(),event.into(),hash.clone().into()]).await?;
    if inserted == 0 {
        let r=txn.query_one(store::stmt("SELECT request_digest FROM communication_inbox_dedupe WHERE source=$1 AND event_id=$2",vec![source.into(),event.into()])).await?.ok_or(Error::Unavailable)?;
        if r.try_get::<Vec<u8>>("", "request_digest")? != hash {
            return Err(Error::Idempotency);
        }
    }
    let _ = s;
    Ok(inserted > 0)
}
pub async fn account_event(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(event): Json<AccountEvent>,
) -> Result<Json<Value>> {
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    if !event.well_formed() {
        return Err(Error::Validation);
    }
    let digest_body = serde_json::to_value(&event).map_err(|_| Error::Validation)?;
    let source = ctx.authenticated_service().as_str().to_owned();
    let mut values = store::scoped(Scope::of(&scope));
    values.push(event.account_reference.clone().into());
    const LOOKUP: &str = "SELECT to_jsonb(p) doc FROM communication_profiles p WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND account_reference=$4";
    let existing: Option<Profile> = one(&s.store.db, LOOKUP, values.clone()).await?;
    if event.kind == AccountEventKind::AccountSessionEnded {
        return session_ended(&s, &scope, &source, &event, &digest_body, existing).await;
    }
    // ACC-010: an account that never opened the Hub has no profile, but it may still
    // hold Messaging and Calling credentials (the web bridge, the BFF). Account owns the
    // principal mapping, so ask it before the transaction (Account is the caller and
    // retries this event until it is answered).
    let unprofiled_principal = if existing.is_none() && event.revokes_credentials() {
        let status = auth::account_status(&s, &scope, &event.account_reference).await?;
        match status["principalId"].as_str() {
            Some(p) => Some(Uuid::parse_str(p).map_err(|_| Error::Unavailable)?),
            None => None,
        }
    } else {
        None
    };
    let txn = s.store.begin().await?;
    if !inbox(&s, &txn, &source, event.event_id, &digest_body).await? {
        txn.commit().await?;
        return Ok(Json(json!({"applied":false,"duplicate":true})));
    }
    let profile: Option<Profile> = one(&txn, &format!("{LOOKUP} FOR UPDATE"), values).await?;
    let mut applied = false;
    let mut fan_out = "NONE";
    if let Some(p) = profile {
        if p.account_version < event.source_version {
            let next = if p.activation_state == Activation::Deactivated {
                "DEACTIVATED"
            } else {
                match event.kind {
                    AccountEventKind::SessionRevoked | AccountEventKind::AccountSessionEnded => {
                        if p.activation_state == Activation::Suspended {
                            "SUSPENDED"
                        } else {
                            "ACCOUNT_CREATED"
                        }
                    }
                    AccountEventKind::StatusChanged => match event.state {
                        Activation::Suspended => "SUSPENDED",
                        Activation::Deactivated => "DEACTIVATED",
                        _ => "ACCOUNT_CREATED",
                    },
                }
            };
            let revoke_credentials = event.revokes_credentials();
            execute(&txn,"UPDATE communication_profiles SET activation_state=$2,account_version=$3,credentials_not_before=CASE WHEN $4 THEN GREATEST(COALESCE(credentials_not_before,$5),$5) ELSE credentials_not_before END,suspended_at=CASE WHEN $2='SUSPENDED' THEN $5 ELSE suspended_at END,deactivated_at=CASE WHEN $2='DEACTIVATED' THEN $5 ELSE deactivated_at END,version=version+1,updated_at=now() WHERE id=$1",vec![p.id.into(),next.into(),event.source_version.into(),revoke_credentials.into(),event.occurred_at.into()]).await?;
            s.store
                .audit_system(
                    &txn,
                    p.scope(),
                    "ACCOUNT_AUTHORITY_CHANGED",
                    Some(p.id),
                    json!({"kind":event.kind,"state":next,"sourceVersion":event.source_version}),
                )
                .await?;
            s.store.event(&txn,&p,"communications.account.changed",p.id,p.version+1,json!({"principalId":p.principal_id,"state":next,"credentialsNotBefore":if revoke_credentials {Some(event.occurred_at.timestamp())} else {None}})).await?;
            applied = true;
            fan_out = "PROFILE";
        }
    } else if let Some(principal) = unprofiled_principal {
        // No Hub profile: the same durable, deduplicated revocation to Calling and
        // Messaging, keyed by the principal. Owners keep the greatest epoch, so replays
        // and reordering are harmless. Nothing else about the account is recorded here;
        // a profile created later takes Account's epoch at activation.
        let state = match event.state {
            Activation::Suspended => "SUSPENDED",
            Activation::Deactivated => "DEACTIVATED",
            _ => "ACCOUNT_CREATED",
        };
        s.store
            .scoped_event(
                &txn,
                Scope::of(&scope),
                "communications.account.changed",
                principal,
                event.source_version,
                json!({"principalId":principal,"state":state,"credentialsNotBefore":event.occurred_at.timestamp(),"profile":false}),
            )
            .await?;
        s.store
            .audit_system(
                &txn,
                Scope::of(&scope),
                "ACCOUNT_REVOCATION_FANNED_OUT",
                Some(principal),
                json!({"kind":event.kind,"state":state,"sourceVersion":event.source_version,"profile":false}),
            )
            .await?;
        applied = true;
        fan_out = "PRINCIPAL";
    }
    txn.commit().await?;
    Ok(Json(
        json!({"applied":applied,"duplicate":false,"fanOut":fan_out}),
    ))
}

/// ACCOUNT_SESSION_ENDED: revoke the Hub devices the ended session linked. Owner calls
/// are idempotent and happen before the deduplication record is written, so a failure
/// answers 503 and Account's durable outbox retries the same event.
async fn session_ended(
    s: &State,
    scope: &DelegatedPlatformContext,
    source: &str,
    event: &AccountEvent,
    digest_body: &Value,
    profile: Option<Profile>,
) -> Result<Json<Value>> {
    let session = event
        .account_session_ref
        .as_deref()
        .ok_or(Error::Validation)?;
    let ended = match &profile {
        Some(p) => s.end_session_devices(scope, p, session).await?,
        None => 0,
    };
    let txn = s.store.begin().await?;
    let fresh = inbox(s, &txn, source, event.event_id, digest_body).await?;
    if fresh {
        if let Some(p) = &profile {
            s.store
                .audit_system(
                    &txn,
                    p.scope(),
                    "ACCOUNT_SESSION_ENDED",
                    Some(p.id),
                    json!({"devicesRevoked":ended}),
                )
                .await?;
        }
    }
    txn.commit().await?;
    Ok(Json(
        json!({"applied":fresh && ended>0,"duplicate":!fresh,"devicesRevoked":ended}),
    ))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockEvent {
    pub event_id: Uuid,
    pub actor_principal_id: Uuid,
    pub target_principal_id: Uuid,
    pub source_version: i64,
    pub blocked: bool,
}
/// DEPRECATED (Blueprint v1.0 Section 14, MSG-016 / DATA-006). No service produces
/// block events: Calling and Messaging stay the authorities for their own block rules and
/// every policy decision reads them live (`State::domain_blocks`). The route is kept,
/// unchanged, for contract compatibility and is removed with the next major version of
/// `cfmeet.communications.communications-internal`; rows it wrote are never read for a
/// decision (`GET /blocks` lists only the Hub's own COMMUNICATIONS blocks).
pub async fn block_event(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(event): Json<BlockEvent>,
) -> Result<Json<Value>> {
    let delegated = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &delegated).await?;
    let scope = Scope::of(&delegated);
    let source = if ctx.authenticated_service() == &ServiceId::calling() {
        "CALLING"
    } else if ctx.authenticated_service() == &ServiceId::messaging() {
        "MESSAGING"
    } else {
        return Err(Error::Forbidden);
    };
    if event.source_version < 1 || event.actor_principal_id == event.target_principal_id {
        return Err(Error::Validation);
    }
    let txn = s.store.begin().await?;
    if !inbox(
        &s,
        &txn,
        ctx.authenticated_service().as_str(),
        event.event_id,
        &serde_json::to_value(&event).map_err(|_| Error::Validation)?,
    )
    .await?
    {
        txn.commit().await?;
        return Ok(Json(json!({"applied":false,"duplicate":true})));
    }
    let a = s
        .store
        .principal(&txn, scope, event.actor_principal_id)
        .await;
    let b = s
        .store
        .principal(&txn, scope, event.target_principal_id)
        .await;
    let applied = match (a, b) {
        (Ok(a), Ok(b)) => {
            let _ = s.store.lock_pair(&txn, scope, a.id, b.id).await?;
            execute(&txn,"INSERT INTO communication_block_bridge(blocker_profile_id,blocked_profile_id,source,active,source_version) VALUES($1,$2,$3,$4,$5) ON CONFLICT(blocker_profile_id,blocked_profile_id,source) DO UPDATE SET active=$4,source_version=$5,updated_at=now() WHERE communication_block_bridge.source_version<$5",vec![a.id.into(),b.id.into(),source.into(),event.blocked.into(),event.source_version.into()]).await?>0
        }
        (Err(Error::NotFound), _) | (_, Err(Error::NotFound)) => false,
        (Err(e), _) | (_, Err(e)) => return Err(e),
    };
    // Missing lazy profiles do not remove canonical domain blocks: every critical
    // decision also reads those blocks live from their owning service.
    txn.commit().await?;
    Ok(Json(json!({"applied":applied,"duplicate":false})))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Backfill {
    limit: u64,
    cursor: Option<Uuid>,
}
pub async fn backfill(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<Backfill>,
) -> Result<Json<Value>> {
    if !(1..=100).contains(&input.limit) {
        return Err(Error::Validation);
    }
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let req = OutboundServiceRequest::new_operation(ServiceId::meeting_service())
        .with_delegated_context(scope.clone());
    let r = s
        .config
        .client
        .post_json(
            &req,
            "/internal/v1/accounts/communications/backfill",
            &json!({"limit":input.limit,"cursor":input.cursor}),
        )
        .await
        .map_err(|_| Error::Unavailable)?;
    if r.status() != 200 {
        return Err(Error::Unavailable);
    }
    let page: Value = r.json().map_err(|_| Error::Unavailable)?;
    let accounts = page["accounts"]
        .as_array()
        .filter(|v| v.len() <= input.limit as usize)
        .ok_or(Error::Unavailable)?;
    let mut count = 0;
    for account in accounts {
        let reference = account["accountReference"]
            .as_str()
            .filter(|v| v.starts_with("cfmeet-user:"))
            .ok_or(Error::Unavailable)?;
        // Account owns the mapping. It reuses its existing principal link and ensures
        // missing native mappings through Platform Access; the Hub never replaces it.
        let principal = uuid_field(account, "principalId")?;
        let txn = s.store.begin().await?;
        let profile = Uuid::new_v4();
        let mut values = vec![profile.into()];
        values.extend(store::scoped(Scope::of(&scope)));
        values.extend([
            reference.into(),
            principal.into(),
            account["displayName"]
                .as_str()
                .unwrap_or("CF Meet member")
                .chars()
                .take(160)
                .collect::<String>()
                .into(),
        ]);
        let created=execute(&txn,"INSERT INTO communication_profiles(id,tenant_id,application_id,environment_id,account_reference,principal_id,display_name) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",values).await?;
        if created > 0 {
            execute(
                &txn,
                "INSERT INTO communication_privacy(profile_id) VALUES($1)",
                vec![profile.into()],
            )
            .await?;
            s.new_public_id(&txn, profile).await?;
            s.store
                .audit_system(
                    &txn,
                    Scope::of(&scope),
                    "PROFILE_BACKFILLED",
                    Some(profile),
                    json!({"source":"ACCOUNT"}),
                )
                .await?;
            count += 1;
        }
        txn.commit().await?;
    }
    Ok(Json(
        json!({"created":count,"nextCursor":page["nextCursor"]}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Replay {
    event_id: Uuid,
}
pub async fn replay(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(input): Json<Replay>,
) -> Result<Json<Value>> {
    let scope = auth::delegated(&ctx)?;
    auth::validate_scope(&s, &scope).await?;
    let mut v = store::scoped(Scope::of(&scope));
    v.push(input.event_id.into());
    let txn = s.store.begin().await?;
    let n=execute(&txn,"UPDATE communication_outbox SET state='PENDING',attempts=0,next_attempt_at=now(),lease_id=NULL,lease_until=NULL WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3 AND event_id=$4 AND state='DEAD'",v).await?;
    s.store
        .audit_system(
            &txn,
            Scope::of(&scope),
            "OUTBOX_REPLAY_REQUESTED",
            Some(input.event_id),
            json!({"requeued":n}),
        )
        .await?;
    txn.commit().await?;
    Ok(Json(json!({"requeued":n})))
}

impl State {
    pub async fn deliver_due(&self) -> Result<usize> {
        execute(&self.store.db,"UPDATE communication_outbox SET state='DEAD',lease_id=NULL,lease_until=NULL WHERE state='INFLIGHT' AND lease_until<=now() AND attempts>=12",vec![]).await?;
        let txn = self.store.begin().await?;
        let lease = Uuid::new_v4();
        let jobs:Vec<Value>=rows(&txn,"WITH due AS (SELECT seq FROM communication_outbox WHERE (state='PENDING' AND next_attempt_at<=now()) OR (state='INFLIGHT' AND lease_until<=now()) ORDER BY seq LIMIT 20 FOR UPDATE SKIP LOCKED) UPDATE communication_outbox o SET state='INFLIGHT',attempts=attempts+1,lease_id=$1,lease_until=now()+interval '90 seconds' FROM due WHERE o.seq=due.seq RETURNING to_jsonb(o) doc",vec![lease.into()]).await?;
        txn.commit().await?;
        let mut delivered = 0;
        for job in jobs {
            let seq = job["seq"].as_i64().ok_or(Error::Unavailable)?;
            let service = match job["destination"].as_str() {
                Some("CALLING") => ServiceId::calling(),
                Some("MESSAGING") => ServiceId::messaging(),
                _ => return Err(Error::Unavailable),
            };
            // Delegation uses the authenticated first-party service account from the
            // durable event's producer scope, persisted with the event (not a secret).
            let scope_value:Value=one(&self.store.db,"SELECT jsonb_build_object('serviceAccountId',service_account_id,'environmentKind',environment_kind) doc FROM communication_scope_authorities WHERE tenant_id=$1 AND application_id=$2 AND environment_id=$3",vec![uuid_field(&job,"tenant_id")?.into(),uuid_field(&job,"application_id")?.into(),uuid_field(&job,"environment_id")?.into()]).await?.ok_or(Error::Unavailable)?;
            let kind: cf_service_auth::ApplicationEnvironmentKind =
                serde_json::from_value(scope_value["environmentKind"].clone())
                    .map_err(|_| Error::Unavailable)?;
            let scope = DelegatedPlatformContext::new(
                uuid_field(&job, "tenant_id")?,
                uuid_field(&job, "application_id")?,
                uuid_field(&job, "environment_id")?,
                kind,
                uuid_field(&scope_value, "serviceAccountId")?,
            );
            let req = OutboundServiceRequest::new_operation(service).with_delegated_context(scope);
            let payload = json!({"schemaVersion":1,"eventId":job["event_id"],"aggregateId":job["aggregate_id"],"version":job["aggregate_version"],"type":job["event_type"],"data":job["payload"]});
            let ok = self
                .config
                .client
                .post_json(&req, "/internal/v1/communications/events", &payload)
                .await
                .is_ok_and(|r| r.status() == 200);
            let attempts = job["attempts"].as_i64().unwrap_or(12);
            if ok {
                execute(&self.store.db,"UPDATE communication_outbox SET state='DELIVERED',delivered_at=now(),lease_id=NULL,lease_until=NULL WHERE seq=$1 AND lease_id=$2",vec![seq.into(),lease.into()]).await?;
                self.metrics.events.fetch_add(1, Ordering::Relaxed);
                delivered += 1;
            } else {
                execute(&self.store.db,"UPDATE communication_outbox SET state=$3,next_attempt_at=now()+make_interval(secs=>$4),lease_id=NULL,lease_until=NULL WHERE seq=$1 AND lease_id=$2",vec![seq.into(),lease.into(),if attempts>=12 {"DEAD"} else {"PENDING"}.into(),(2i32.pow(attempts.min(10) as u32)).into()]).await?;
            }
        }
        Ok(delivered)
    }
    pub async fn maintain(&self) -> Result<()> {
        // Every sweep is bounded. Identity/public-id tombstones and undelivered events
        // survive application rollback and are never silently purged.
        for sql in [
            "DELETE FROM communication_service_nonces WHERE ctid IN(SELECT ctid FROM communication_service_nonces WHERE expires_at<now() LIMIT 1000)",
            "DELETE FROM communication_idempotency WHERE ctid IN(SELECT ctid FROM communication_idempotency WHERE expires_at<now() LIMIT 1000)",
            "DELETE FROM communication_rate_windows WHERE ctid IN(SELECT ctid FROM communication_rate_windows WHERE window_start<now()-interval '2 days' LIMIT 1000)",
            "DELETE FROM communication_outbox WHERE seq IN(SELECT seq FROM communication_outbox WHERE state='DELIVERED' AND delivered_at<now()-interval '30 days' LIMIT 1000)",
            "DELETE FROM communication_audit WHERE seq IN(SELECT seq FROM communication_audit WHERE created_at<now()-interval '365 days' LIMIT 1000)",
            "DELETE FROM communication_profile_reports WHERE id IN(SELECT id FROM communication_profile_reports WHERE created_at<now()-interval '365 days' LIMIT 1000)",
            "DELETE FROM communication_changes WHERE seq IN(SELECT seq FROM communication_changes WHERE created_at<now()-interval '30 days' LIMIT 1000)",
            // MSG-009: owed call cards and nudge deduplication keys are bounded too (call
            // history keeps listing the call; cards only refresh during their first day).
            "DELETE FROM communication_pending_call_cards WHERE ctid IN(SELECT ctid FROM communication_pending_call_cards WHERE created_at<now()-interval '30 days' LIMIT 1000)",
            "DELETE FROM communication_timeline_nudges WHERE ctid IN(SELECT ctid FROM communication_timeline_nudges WHERE created_at<now()-interval '30 days' LIMIT 1000)",
        ] {execute(&self.store.db,sql,vec![]).await?;}
        let txn = self.store.begin().await?;
        let pairs:Vec<Value>=rows(&txn,"SELECT to_jsonb(c) doc FROM communication_connections c WHERE state='REQUEST_PENDING' AND EXISTS(SELECT 1 FROM communication_contact_requests r WHERE r.connection_id=c.id AND r.state='PENDING' AND r.expires_at<=now()) ORDER BY c.id LIMIT 100 FOR UPDATE OF c SKIP LOCKED",vec![]).await?;
        for pair in pairs {
            let id = uuid_field(&pair, "id")?;
            let count=execute(&txn,"UPDATE communication_contact_requests SET state='EXPIRED',version=version+1,updated_at=now() WHERE connection_id=$1 AND state='PENDING' AND expires_at<=now()",vec![id.into()]).await?;
            if count > 0 {
                execute(&txn,"UPDATE communication_connections SET state='NONE',version=version+1,updated_at=now() WHERE id=$1 AND state='REQUEST_PENDING'",vec![id.into()]).await?;
                let scope = Scope {
                    tenant_id: uuid_field(&pair, "tenant_id")?,
                    application_id: uuid_field(&pair, "application_id")?,
                    environment_id: uuid_field(&pair, "environment_id")?,
                };
                let p = self
                    .store
                    .profile(&txn, scope, uuid_field(&pair, "left_profile_id")?, false)
                    .await?;
                self.store
                    .audit_system(
                        &txn,
                        scope,
                        "CONNECTION_REQUEST_EXPIRED",
                        Some(id),
                        json!({}),
                    )
                    .await?;
                self.store
                    .event(
                        &txn,
                        &p,
                        "communications.connection.expired",
                        id,
                        pair["version"].as_i64().ok_or(Error::Unavailable)? + 1,
                        json!({"state":"NONE"}),
                    )
                    .await?;
            }
        }
        txn.commit().await?;
        Ok(())
    }
}
