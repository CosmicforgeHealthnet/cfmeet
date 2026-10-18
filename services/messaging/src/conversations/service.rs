//! Conversation application service (Part V D.5 / D.7–D.10 / D.12).
//!
//! Every state change runs in ONE transaction with its outbox event; concurrency is
//! settled by the database (unique indexes for direct pairs and context references,
//! `FOR UPDATE` + aggregate version for lifecycle changes, idempotency rows for retried
//! creations). No client timestamp ever orders writes.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseTransaction, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    policy::{self, canonical_json, ApplicationPolicy, SECURITY_POLICY_VERSION},
    ContextClass, ContextRef, LifecycleState,
};
use crate::{
    audit::{self, AuditEntry},
    db,
    domain::{current_context, RequestContext, SecurityProfile},
    error::{Error, ErrorCode, Result},
    events::outbox::{self, EventScope, NewEvent},
    http::middleware::Acting,
    identity::{PrincipalStatus, TrustedContext},
    limits::{self, Window},
    state::AppState,
};

fn request_context() -> RequestContext {
    current_context().unwrap_or_else(RequestContext::generate)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn scope_of(ctx: &TrustedContext) -> EventScope {
    EventScope {
        tenant_id: ctx.scope.tenant_id,
        application_id: ctx.scope.application_id,
        environment_id: ctx.scope.environment_id,
    }
}

// ---------------------------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
pub struct ConversationRow {
    pub id: Uuid,
    pub context_class: String,
    pub context_namespace: Option<String>,
    pub context_ref: Option<String>,
    pub context_ref_version: Option<i32>,
    pub security_profile: String,
    pub protocol: String,
    pub ciphersuite: Option<String>,
    pub mls_group_id: Option<Vec<u8>>,
    pub security_policy_version: i32,
    pub lifecycle_state: String,
    pub lifecycle_reason: Option<String>,
    pub policy_snapshot_id: Uuid,
    pub policy_version: i64,
    pub policy_digest: String,
    pub version: i64,
    pub created_by_principal_id: Uuid,
    pub owner_principal_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub state_changed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "c.id, c.context_class, c.context_namespace, c.context_ref, c.context_ref_version, \
     c.security_profile, c.protocol, c.ciphersuite, c.mls_group_id, c.security_policy_version, \
     c.lifecycle_state, c.lifecycle_reason, c.policy_snapshot_id, c.policy_version, s.digest AS policy_digest, \
     c.version, c.created_by_principal_id, c.owner_principal_id, c.created_at, c.updated_at, \
     c.state_changed_at, c.expires_at";

const FROM: &str =
    "messaging_conversations c JOIN messaging_policy_snapshots s ON s.id = c.policy_snapshot_id";

#[derive(Debug, Clone, FromQueryResult)]
struct MemberRow {
    principal_id: Uuid,
    role: String,
}

impl ConversationRow {
    pub fn state(&self) -> LifecycleState {
        LifecycleState::parse(&self.lifecycle_state).unwrap_or(LifecycleState::Deleted)
    }

    pub fn profile(&self) -> SecurityProfile {
        SecurityProfile::parse(&self.security_profile).unwrap_or(SecurityProfile::PrivateE2ee)
    }
}

fn view(c: &ConversationRow, members: &[MemberRow]) -> Value {
    let profile = c.profile();
    json!({
        "conversationId": c.id,
        "contextClass": c.context_class,
        "context": match (&c.context_namespace, &c.context_ref) {
            (Some(ns), Some(r)) => json!({ "namespace": ns, "reference": r, "version": c.context_ref_version }),
            _ => Value::Null,
        },
        "securityProfile": c.security_profile,
        "encryption": {
            "protocol": c.protocol,
            "ciphersuite": c.ciphersuite,
            "mlsGroupId": c.mls_group_id.as_ref().map(|g| URL_SAFE_NO_PAD.encode(g)),
            "securityPolicyVersion": c.security_policy_version,
            "serverCanReadContent": profile.server_may_process_content(),
        },
        "lifecycleState": c.lifecycle_state,
        "lifecycleReason": c.lifecycle_reason,
        "policy": { "snapshotId": c.policy_snapshot_id, "digest": c.policy_digest, "applicationPolicyVersion": c.policy_version },
        "version": c.version,
        "createdByPrincipalId": c.created_by_principal_id,
        "ownerPrincipalId": c.owner_principal_id,
        "members": members.iter().map(|m| json!({ "principalId": m.principal_id, "role": m.role })).collect::<Vec<_>>(),
        "createdAt": rfc3339(c.created_at),
        "updatedAt": rfc3339(c.updated_at),
        "stateChangedAt": rfc3339(c.state_changed_at),
        "expiresAt": c.expires_at.map(rfc3339),
    })
}

async fn load<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    id: Uuid,
    lock: bool,
) -> Result<Option<ConversationRow>> {
    Ok(db::one(
        db,
        &format!(
            "SELECT {COLUMNS} FROM {FROM} WHERE c.id = $1 AND c.tenant_id = $2 AND c.application_id = $3 \
               AND c.environment_id = $4{}",
            if lock { " FOR UPDATE OF c" } else { "" }
        ),
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
        ],
    )
    .await?)
}

async fn members<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Vec<MemberRow>> {
    Ok(db::all(
        db,
        "SELECT principal_id, role FROM messaging_conversation_members WHERE conversation_id = $1 ORDER BY role DESC, principal_id",
        vec![id.into()],
    )
    .await?)
}

async fn full_view<C: ConnectionTrait>(db: &C, row: &ConversationRow) -> Result<Value> {
    Ok(view(row, &members(db, row.id).await?))
}

// ---------------------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    pub context_class: ContextClass,
    #[serde(default)]
    pub security_profile: Option<SecurityProfile>,
    /// DIRECT only: the other principal.
    #[serde(default)]
    pub counterpart_principal_id: Option<Uuid>,
    #[serde(default)]
    pub context: Option<ContextRef>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Optional optimistic guard: the application policy version the caller decided on.
    #[serde(default)]
    pub expected_policy_version: Option<i64>,
}

/// `(HTTP status, body)` of a creation, as stored for idempotent replays.
pub struct Created {
    pub status: u16,
    pub body: Value,
}

pub fn valid_idempotency_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn direct_pair(a: Uuid, b: Uuid) -> String {
    let (x, y) = if a < b { (a, b) } else { (b, a) };
    format!("{}:{}", x.hyphenated(), y.hyphenated())
}

pub async fn create(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    request: CreateRequest,
    idempotency_key: &str,
) -> Result<Created> {
    if !valid_idempotency_key(idempotency_key) {
        return Err(Error::validation(
            "Idempotency-Key must be 8-128 characters of [A-Za-z0-9._:-]",
        ));
    }
    if let Some(c) = &request.context {
        c.validate().map_err(Error::validation)?;
    }
    let counterpart = match (request.context_class, request.counterpart_principal_id) {
        (ContextClass::Direct, Some(p)) if p == acting.principal_id => {
            return Err(Error::validation("a DIRECT conversation needs two different principals"))
        }
        (ContextClass::Direct, Some(p)) => Some(p),
        (ContextClass::Direct, None) => {
            return Err(Error::validation("DIRECT conversations need counterpartPrincipalId"))
        }
        (_, Some(_)) => {
            return Err(Error::validation(
                "counterpartPrincipalId applies to DIRECT only; members are managed by the membership API",
            ))
        }
        (_, None) => None,
    };
    if let Some(p) = counterpart {
        // The counterpart must be an ACTIVE messaging principal of the SAME environment.
        match state.principals.resolve(&ctx.scope, p).await {
            Ok(s @ PrincipalStatus::Active { .. }) if s.is_messaging_capable() => {}
            Ok(_) => {
                return Err(Error::reason(
                    ErrorCode::PrincipalNotAuthorized,
                    "COUNTERPART_NOT_AVAILABLE",
                ))
            }
            Err(e) => {
                tracing::warn!(error = %e, "principal directory unavailable");
                return Err(Error::code(ErrorCode::ServiceUnavailable));
            }
        }
    }
    let now = state.now();
    let key = format!(
        "conversation-create:{}:{}",
        ctx.scope.environment_id.simple(),
        acting.principal_id.simple()
    );
    if let Some(retry) = limits::hit(
        &state.db,
        &key,
        Window::Minute,
        state.conversations.creations_per_minute,
        now,
    )
    .await?
    {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let digest = hex::encode(Sha256::digest(
        canonical_json(&json!({
            "request": serde_json::to_value(&request).expect("serializes"),
            "actingPrincipalId": acting.principal_id,
        }))
        .as_bytes(),
    ));
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.clone();
    let key = idempotency_key.to_string();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, Created, Error>(move |txn| {
            Box::pin(async move {
                create_in(
                    &st,
                    txn,
                    &ctx,
                    &acting,
                    request,
                    counterpart,
                    &key,
                    &digest,
                    &request_ctx,
                )
                .await
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(FromQueryResult)]
struct IdempotencyRow {
    request_digest: String,
    response_status: Option<i16>,
    response_view: Option<Value>,
}

#[allow(clippy::too_many_arguments)]
async fn create_in(
    state: &AppState,
    txn: &DatabaseTransaction,
    ctx: &TrustedContext,
    acting: &Acting,
    request: CreateRequest,
    counterpart: Option<Uuid>,
    key: &str,
    digest: &str,
    request_ctx: &RequestContext,
) -> Result<Created> {
    let now = state.now();
    let env = ctx.scope.environment_id;
    // Claim the idempotency key first: a concurrent retry with the same key waits here for
    // this transaction and then replays its stored response.
    let claimed = db::exec(
        txn,
        "INSERT INTO messaging_idempotency_keys (environment_id, idempotency_key, operation, request_digest, \
           created_at, expires_at) VALUES ($1, $2, 'conversation.create', $3, $4, $5) ON CONFLICT DO NOTHING",
        vec![env.into(), key.into(), digest.into(), now.into(), (now + chrono::Duration::hours(24)).into()],
    )
    .await?;
    if claimed == 0 {
        let row: IdempotencyRow = db::one(
            txn,
            "SELECT request_digest, response_status, response_view FROM messaging_idempotency_keys \
             WHERE environment_id = $1 AND operation = 'conversation.create' AND idempotency_key = $2",
            vec![env.into(), key.into()],
        )
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))?;
        if row.request_digest != digest {
            return Err(Error::code(ErrorCode::IdempotencyKeyReused));
        }
        return match (row.response_status, row.response_view) {
            (Some(_), Some(body)) => Ok(Created { status: 200, body }),
            _ => Err(Error::code(ErrorCode::Conflict)),
        };
    }

    let snapshot = policy::current_snapshot(txn, env, now).await?;
    if request
        .expected_policy_version
        .is_some_and(|v| v != snapshot.policy.application_policy_version)
    {
        return Err(Error::code(ErrorCode::PolicyVersionConflict));
    }
    let context_policy = snapshot.policy.context(request.context_class).clone();
    if !context_policy.enabled {
        return Err(Error::code(ErrorCode::ContextNotAllowed));
    }
    // Profile selection: the request or the persisted default — never a fallback.
    let profile = match request.security_profile.or(context_policy.default_profile) {
        Some(p) => p,
        None => {
            return Err(Error::reason(
                ErrorCode::SecurityProfileNotAllowed,
                "NO_DEFAULT_PROFILE",
            ))
        }
    };
    if !context_policy.allowed_profiles.contains(&profile) {
        return Err(Error::reason(
            ErrorCode::SecurityProfileNotAllowed,
            match profile {
                SecurityProfile::Managed => "MANAGED_NOT_ALLOWED",
                SecurityProfile::PrivateE2ee => "PRIVATE_E2EE_NOT_ALLOWED",
            },
        ));
    }
    if profile == SecurityProfile::PrivateE2ee {
        // An encrypted conversation is created only when it can actually be encrypted;
        // otherwise a clear error — never a MANAGED conversation instead.
        if !state.devices.enabled {
            return Err(Error::reason(
                ErrorCode::E2eeNotAvailable,
                "E2EE_DEVICES_DISABLED",
            ));
        }
        for (principal, reason) in std::iter::once((acting.principal_id, "CREATOR_HAS_NO_DEVICE"))
            .chain(counterpart.map(|p| (p, "COUNTERPART_HAS_NO_DEVICE")))
        {
            let devices = db::scalar_i64(
                txn,
                "SELECT count(*) FROM messaging_devices WHERE environment_id = $1 AND principal_id = $2 AND status = 'ACTIVE'",
                vec![env.into(), principal.into()],
            )
            .await?;
            if devices == 0 {
                return Err(Error::reason(ErrorCode::E2eeNotAvailable, reason));
            }
        }
    }
    let expires_at = match (request.expires_at, context_policy.max_lifetime_seconds) {
        (Some(t), _) if t <= now => {
            return Err(Error::validation("expiresAt must be in the future"))
        }
        (Some(t), Some(max)) if t > now + chrono::Duration::seconds(max) => {
            return Err(Error::validation(
                "expiresAt exceeds the context's maximum lifetime",
            ))
        }
        (Some(t), _) => Some(t),
        (None, Some(max)) => Some(now + chrono::Duration::seconds(max)),
        (None, None) => None,
    };

    let id = Uuid::new_v4();
    let mls_group_id = (profile == SecurityProfile::PrivateE2ee).then(|| {
        let mut g = vec![0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut g);
        g
    });
    let pair = counterpart.map(|p| direct_pair(acting.principal_id, p));
    let inserted = db::exec(
        txn,
        "INSERT INTO messaging_conversations (id, tenant_id, application_id, environment_id, context_class, \
           context_namespace, context_ref, context_ref_version, security_profile, protocol, ciphersuite, mls_group_id, \
           security_policy_version, lifecycle_state, policy_snapshot_id, policy_version, created_by_principal_id, \
           created_by_credential_id, owner_principal_id, direct_pair_key, created_at, updated_at, state_changed_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'ACTIVE', $14, $15, $16, $17, $16, $18, $19, $19, $19, $20) \
         ON CONFLICT DO NOTHING",
        vec![
            id.into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            env.into(),
            request.context_class.as_str().into(),
            request.context.as_ref().map(|c| c.namespace.clone()).into(),
            request.context.as_ref().map(|c| c.reference.clone()).into(),
            request.context.as_ref().map(|c| c.version).into(),
            profile.as_str().into(),
            profile.protocol().into(),
            (profile == SecurityProfile::PrivateE2ee).then_some(cf_messaging_crypto::profile::CIPHERSUITE_NAME).into(),
            mls_group_id.into(),
            SECURITY_POLICY_VERSION.into(),
            snapshot.id.into(),
            snapshot.policy.application_policy_version.into(),
            acting.principal_id.into(),
            ctx.credential_id.into(),
            pair.clone().into(),
            now.into(),
            expires_at.into(),
        ],
    )
    .await?;
    let (row, status) = if inserted == 1 {
        db::exec(
            txn,
            "INSERT INTO messaging_conversation_members (conversation_id, principal_id, environment_id, role, joined_at) \
             VALUES ($1, $2, $3, 'OWNER', $4)",
            vec![id.into(), acting.principal_id.into(), env.into(), now.into()],
        )
        .await?;
        if let Some(p) = counterpart {
            db::exec(
                txn,
                "INSERT INTO messaging_conversation_members (conversation_id, principal_id, environment_id, role, joined_at) \
                 VALUES ($1, $2, $3, 'MEMBER', $4)",
                vec![id.into(), p.into(), env.into(), now.into()],
            )
            .await?;
        }
        db::exec(
            txn,
            "INSERT INTO messaging_conversation_sequences (conversation_id) VALUES ($1)",
            vec![id.into()],
        )
        .await?;
        outbox::enqueue(
            txn,
            NewEvent {
                event_type: "messaging.conversation.created",
                aggregate_type: "CONVERSATION",
                aggregate_id: id,
                scope: scope_of(ctx),
                payload: json!({
                    "conversation_id": id,
                    "conversation_kind": request.context_class.as_str(),
                    "security_profile": profile.as_str(),
                    "lifecycle_state": "ACTIVE",
                    "version": 1,
                    "policy_digest": snapshot.digest,
                    "created_by_principal_id": acting.principal_id,
                }),
            },
            request_ctx,
            now,
        )
        .await?;
        crate::metrics::inc(
            "messaging_conversations_created_total",
            &[("profile", profile.as_str())],
        );
        (
            load(txn, ctx, id, false)
                .await?
                .ok_or(Error::code(ErrorCode::InternalError))?,
            201,
        )
    } else {
        // A live conversation already holds this direct pair or context reference: return
        // it when it is the same kind of conversation, never merge different profiles.
        let existing: Option<ConversationRow> = if let Some(pair) = &pair {
            db::one(
                txn,
                &format!(
                    "SELECT {COLUMNS} FROM {FROM} WHERE c.environment_id = $1 AND c.context_class = 'DIRECT' \
                       AND c.security_profile = $2 AND c.direct_pair_key = $3 \
                       AND c.lifecycle_state IN ('ACTIVE', 'LOCKED', 'CLOSED', 'ARCHIVED')"
                ),
                vec![env.into(), profile.as_str().into(), pair.clone().into()],
            )
            .await?
        } else if let Some(c) = &request.context {
            db::one(
                txn,
                &format!(
                    "SELECT {COLUMNS} FROM {FROM} WHERE c.environment_id = $1 AND c.context_class = $2 \
                       AND c.context_namespace = $3 AND c.context_ref = $4 \
                       AND c.lifecycle_state IN ('ACTIVE', 'LOCKED', 'CLOSED', 'ARCHIVED')"
                ),
                vec![env.into(), request.context_class.as_str().into(), c.namespace.clone().into(), c.reference.clone().into()],
            )
            .await?
        } else {
            None
        };
        match existing {
            Some(row) if row.profile() == profile => (row, 200),
            Some(_) => {
                return Err(Error::reason(
                    ErrorCode::Conflict,
                    "CONTEXT_HELD_BY_OTHER_PROFILE",
                ))
            }
            None => return Err(Error::code(ErrorCode::Conflict)),
        }
    };
    let body = full_view(txn, &row).await?;
    db::exec(
        txn,
        "UPDATE messaging_idempotency_keys SET response_status = $4, response_view = $5 \
         WHERE environment_id = $1 AND operation = 'conversation.create' AND idempotency_key = $2 AND request_digest = $3",
        vec![env.into(), key.into(), digest.into(), (status as i16).into(), body.clone().into()],
    )
    .await?;
    Ok(Created { status, body })
}

// ---------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------

/// Who reads: a member principal, or the application's administration (no principal).
#[derive(Debug, Clone)]
pub enum Reader {
    Member(Acting),
    Administration,
}

async fn is_member<C: ConnectionTrait>(
    db: &C,
    conversation: Uuid,
    principal: Uuid,
) -> Result<bool> {
    Ok(db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_conversation_members WHERE conversation_id = $1 AND principal_id = $2",
        vec![conversation.into(), principal.into()],
    )
    .await?
        > 0)
}

pub async fn get(
    state: &AppState,
    ctx: &TrustedContext,
    reader: &Reader,
    id: Uuid,
) -> Result<Value> {
    let row = load(&state.db, ctx, id, false)
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    if let Reader::Member(acting) = reader {
        // A non-member is told nothing (indistinguishable from a missing conversation).
        if !is_member(&state.db, id, acting.principal_id).await? {
            return Err(Error::code(ErrorCode::NotFound));
        }
    }
    full_view(&state.db, &row).await
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    pub lifecycle_state: Option<String>,
    #[serde(default)]
    pub context_class: Option<String>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

fn encode_cursor(t: DateTime<Utc>, id: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}|{}", t.timestamp_micros(), id))
}

fn decode_cursor(c: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(c).ok()?).ok()?;
    let (t, id) = raw.split_once('|')?;
    Some((
        DateTime::from_timestamp_micros(t.parse().ok()?)?,
        Uuid::parse_str(id).ok()?,
    ))
}

/// Keyset pagination, newest first, at most 100 per page.
pub async fn list(
    state: &AppState,
    ctx: &TrustedContext,
    reader: &Reader,
    q: ListQuery,
) -> Result<Value> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 100"));
    }
    let state_filter = match q.lifecycle_state.as_deref() {
        None => None,
        Some(s) => Some(
            LifecycleState::parse(s).ok_or_else(|| Error::validation("unknown lifecycleState"))?,
        ),
    };
    let class_filter = match q.context_class.as_deref() {
        None => None,
        Some(s) => {
            Some(ContextClass::parse(s).ok_or_else(|| Error::validation("unknown contextClass"))?)
        }
    };
    let cursor = match q.cursor.as_deref() {
        None => None,
        Some(c) => Some(decode_cursor(c).ok_or_else(|| Error::validation("invalid cursor"))?),
    };
    let (cursor_t, cursor_id) = cursor
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    let member = match reader {
        Reader::Member(a) => Some(a.principal_id),
        Reader::Administration => None,
    };
    let rows: Vec<ConversationRow> = db::all(
        &state.db,
        &format!(
            "SELECT {COLUMNS} FROM {FROM} \
             WHERE c.tenant_id = $1 AND c.application_id = $2 AND c.environment_id = $3 \
               AND ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM messaging_conversation_members m \
                    WHERE m.conversation_id = c.id AND m.principal_id = $4)) \
               AND ($5::text IS NULL OR c.lifecycle_state = $5) \
               AND ($6::text IS NULL OR c.context_class = $6) \
               AND ($7::timestamptz IS NULL OR (c.created_at, c.id) < ($7, $8)) \
             ORDER BY c.created_at DESC, c.id DESC LIMIT $9"
        ),
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            member.into(),
            state_filter.map(|s| s.as_str()).into(),
            class_filter.map(|c| c.as_str()).into(),
            cursor_t.into(),
            cursor_id.into(),
            ((limit + 1) as i64).into(),
        ],
    )
    .await?;
    let more = rows.len() as u64 > limit;
    let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = if more {
        page.last().map(|r| encode_cursor(r.created_at, r.id))
    } else {
        None
    };
    let mut items = Vec::with_capacity(page.len());
    for row in &page {
        items.push(full_view(&state.db, row).await?);
    }
    Ok(json!({ "conversations": items, "nextCursor": next }))
}

// ---------------------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchRequest {
    pub expected_version: i64,
    #[serde(default)]
    pub lifecycle_state: Option<LifecycleState>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Accepted only to be refused explicitly: a profile can never change.
    #[serde(default)]
    pub security_profile: Option<Value>,
}

enum PatchOutcome {
    Updated(Value),
    /// The conversation expired before the change could apply (expiry was committed).
    Expired,
}

fn valid_reason(r: &str) -> bool {
    (2..=48).contains(&r.len())
        && r.as_bytes()[0].is_ascii_uppercase()
        && r.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

pub async fn patch(
    state: &AppState,
    ctx: &TrustedContext,
    acting: Option<&Acting>,
    id: Uuid,
    request: PatchRequest,
) -> Result<Value> {
    if request.security_profile.is_some() {
        return Err(Error::code(ErrorCode::SecurityProfileImmutable));
    }
    if request.lifecycle_state.is_none() && request.expires_at.is_none() {
        return Err(Error::validation(
            "nothing to change: lifecycleState or expiresAt is required",
        ));
    }
    if let Some(r) = &request.reason {
        if !valid_reason(r) {
            return Err(Error::validation(
                "reason must be an UPPER_SNAKE_CASE code of 2-48 characters",
            ));
        }
    }
    if request
        .lifecycle_state
        .is_some_and(|s| !s.client_requestable())
    {
        return Err(Error::reason(
            ErrorCode::InvalidTransition,
            "SERVER_ONLY_STATE",
        ));
    }
    let st = state.clone();
    let ctx = ctx.clone();
    let acting = acting.cloned();
    let request_ctx = request_context();
    let outcome = state
        .db
        .transaction::<_, PatchOutcome, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let row = load(txn, &ctx, id, true)
                    .await?
                    .ok_or(Error::code(ErrorCode::NotFound))?;
                if let Some(a) = &acting {
                    // A principal administering a conversation must be its owner.
                    if row.owner_principal_id != a.principal_id {
                        return Err(if is_member(txn, id, a.principal_id).await? {
                            Error::reason(ErrorCode::PrincipalNotAuthorized, "NOT_OWNER")
                        } else {
                            Error::code(ErrorCode::NotFound)
                        });
                    }
                }
                let current = row.state();
                // Expiry is evaluated on the server's clock before anything else.
                if current.expirable() && row.expires_at.is_some_and(|t| t <= now) {
                    transition(
                        txn,
                        &ctx,
                        &row,
                        LifecycleState::Expired,
                        Some("CONTEXT_EXPIRED"),
                        None,
                        now,
                        &request_ctx,
                    )
                    .await?;
                    return Ok(PatchOutcome::Expired);
                }
                if request.expected_version != row.version {
                    return Err(Error::code(ErrorCode::VersionConflict));
                }
                let next = request.lifecycle_state.unwrap_or(current);
                if next != current {
                    if !current.can_become(next) {
                        return Err(Error::reason(
                            ErrorCode::InvalidTransition,
                            "TRANSITION_NOT_ALLOWED",
                        ));
                    }
                    if current == LifecycleState::Closed && next == LifecycleState::Active {
                        let reopen: Option<bool> = policy_flag(
                            txn,
                            row.policy_snapshot_id,
                            &row.context_class,
                            "reopenAllowed",
                        )
                        .await?;
                        if reopen != Some(true) {
                            return Err(Error::reason(
                                ErrorCode::InvalidTransition,
                                "REOPEN_NOT_ALLOWED",
                            ));
                        }
                    }
                }
                if let Some(t) = request.expires_at {
                    if t <= now {
                        return Err(Error::validation("expiresAt must be in the future"));
                    }
                }
                let changed = transition(
                    txn,
                    &ctx,
                    &row,
                    next,
                    request.reason.as_deref(),
                    request.expires_at,
                    now,
                    &request_ctx,
                )
                .await?;
                Ok(PatchOutcome::Updated(full_view(txn, &changed).await?))
            })
        })
        .await
        .map_err(Error::from)?;
    match outcome {
        PatchOutcome::Updated(v) => Ok(v),
        PatchOutcome::Expired => Err(Error::reason(ErrorCode::ConversationNotActive, "EXPIRED")),
    }
}

#[derive(FromQueryResult)]
struct FlagRow {
    flag: Option<bool>,
}

async fn policy_flag<C: ConnectionTrait>(
    db: &C,
    snapshot: Uuid,
    class: &str,
    flag: &str,
) -> Result<Option<bool>> {
    let row: Option<FlagRow> = db::one(
        db,
        "SELECT (document -> 'contexts' -> $2 ->> $3)::boolean AS flag FROM messaging_policy_snapshots WHERE id = $1",
        vec![snapshot.into(), class.into(), flag.into()],
    )
    .await?;
    Ok(row.and_then(|r| r.flag))
}

/// Applies a lifecycle / expiry change (version + 1) and records its event.
#[allow(clippy::too_many_arguments)]
async fn transition<C: ConnectionTrait>(
    db: &C,
    ctx: &TrustedContext,
    row: &ConversationRow,
    next: LifecycleState,
    reason: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    request: &RequestContext,
) -> Result<ConversationRow> {
    let state_changed = next != row.state();
    let updated = db::exec(
        db,
        "UPDATE messaging_conversations SET lifecycle_state = $3, \
           lifecycle_reason = CASE WHEN $6 THEN $4 ELSE lifecycle_reason END, \
           expires_at = COALESCE($5, expires_at), version = version + 1, updated_at = $7, \
           state_changed_at = CASE WHEN $6 THEN $7 ELSE state_changed_at END \
         WHERE id = $1 AND version = $2",
        vec![
            row.id.into(),
            row.version.into(),
            next.as_str().into(),
            reason.map(str::to_string).into(),
            expires_at.into(),
            state_changed.into(),
            now.into(),
        ],
    )
    .await
    .map_err(map_trigger)?;
    if updated != 1 {
        return Err(Error::code(ErrorCode::VersionConflict));
    }
    outbox::enqueue(
        db,
        NewEvent {
            event_type: if state_changed {
                "messaging.conversation.lifecycle-changed"
            } else {
                "messaging.conversation.updated"
            },
            aggregate_type: "CONVERSATION",
            aggregate_id: row.id,
            scope: scope_of(ctx),
            payload: json!({
                "conversation_id": row.id,
                "previous_state": row.lifecycle_state,
                "lifecycle_state": next.as_str(),
                "reason": reason,
                "version": row.version + 1,
            }),
        },
        request,
        now,
    )
    .await?;
    load(db, ctx, row.id, false)
        .await?
        .ok_or(Error::code(ErrorCode::InternalError))
}

/// The database's own refusals (defence in depth) as stable codes.
fn map_trigger(e: sea_orm::DbErr) -> Error {
    match crate::db::sqlstate(&e).as_deref() {
        Some("P0002") => Error::code(ErrorCode::SecurityProfileImmutable),
        Some("P0003") => Error::reason(ErrorCode::InvalidTransition, "TRANSITION_NOT_ALLOWED"),
        _ => e.into(),
    }
}

/// Expires due conversations (worker): bounded batches, `SKIP LOCKED` so concurrent
/// instances never expire the same row twice; each expiry commits with its event.
pub async fn expire_due(state: &AppState, batch: i64) -> Result<usize> {
    let st = state.clone();
    state
        .db
        .transaction::<_, usize, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                #[derive(FromQueryResult)]
                struct Due {
                    id: Uuid,
                    tenant_id: Uuid,
                    application_id: Uuid,
                    environment_id: Uuid,
                    lifecycle_state: String,
                    version: i64,
                }
                let due: Vec<Due> = db::all(
                    txn,
                    "SELECT id, tenant_id, application_id, environment_id, lifecycle_state, version FROM messaging_conversations \
                     WHERE expires_at <= $1 AND lifecycle_state IN ('ACTIVE', 'LOCKED', 'CLOSED') \
                     ORDER BY expires_at LIMIT $2 FOR UPDATE SKIP LOCKED",
                    vec![now.into(), batch.into()],
                )
                .await?;
                let request = RequestContext::generate();
                for d in &due {
                    db::exec(
                        txn,
                        "UPDATE messaging_conversations SET lifecycle_state = 'EXPIRED', lifecycle_reason = 'CONTEXT_EXPIRED', \
                           version = version + 1, updated_at = $2, state_changed_at = $2 WHERE id = $1",
                        vec![d.id.into(), now.into()],
                    )
                    .await?;
                    outbox::enqueue(
                        txn,
                        NewEvent {
                            event_type: "messaging.conversation.lifecycle-changed",
                            aggregate_type: "CONVERSATION",
                            aggregate_id: d.id,
                            scope: EventScope {
                                tenant_id: d.tenant_id,
                                application_id: d.application_id,
                                environment_id: d.environment_id,
                            },
                            payload: json!({
                                "conversation_id": d.id,
                                "previous_state": d.lifecycle_state,
                                "lifecycle_state": "EXPIRED",
                                "reason": "CONTEXT_EXPIRED",
                                "version": d.version + 1,
                            }),
                        },
                        &request,
                        now,
                    )
                    .await?;
                }
                Ok(due.len())
            })
        })
        .await
        .map_err(Error::from)
}

/// Runs [`expire_due`] until the process stops.
pub async fn run_expiry(state: AppState, interval: std::time::Duration) {
    loop {
        match expire_due(&state, 500).await {
            Ok(n) if n > 0 => tracing::info!(expired = n, "conversations expired"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "conversation expiry pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

// ---------------------------------------------------------------------------------------
// Application policy
// ---------------------------------------------------------------------------------------

pub async fn policy_view(state: &AppState, ctx: &TrustedContext) -> Result<Value> {
    let (overrides, version) =
        policy::load_application_policy(&state.db, ctx.scope.environment_id, false).await?;
    let effective =
        policy::resolve(&overrides, version).map_err(|_| Error::code(ErrorCode::InternalError))?;
    Ok(json!({
        "version": version,
        "policy": overrides,
        "effective": effective.to_value(),
        "digest": effective.digest(),
        "platformPolicyVersion": policy::PLATFORM_POLICY_VERSION,
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyUpdate {
    pub expected_version: i64,
    pub policy: ApplicationPolicy,
}

pub async fn update_policy(
    state: &AppState,
    ctx: &TrustedContext,
    update: PolicyUpdate,
) -> Result<Value> {
    let resolved =
        policy::resolve(&update.policy, update.expected_version + 1).map_err(Error::validation)?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let request_ctx = request_context();
    state
        .db
        .transaction::<_, (), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let env = ctx2.scope.environment_id;
                // Serialize writers of this environment's policy (also when none exists yet).
                db::exec(
                    txn,
                    "SELECT pg_advisory_xact_lock(hashtextextended($1, 11))",
                    vec![format!("messaging-policy:{env}").into()],
                )
                .await?;
                let (_, current) = policy::load_application_policy(txn, env, true).await?;
                if current != update.expected_version {
                    return Err(Error::code(ErrorCode::PolicyVersionConflict));
                }
                let document = serde_json::to_value(&update.policy).expect("serializes");
                db::exec(
                    txn,
                    "INSERT INTO messaging_application_policies (environment_id, tenant_id, application_id, version, \
                       document, updated_by_credential_id, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (environment_id) DO UPDATE SET version = EXCLUDED.version, document = EXCLUDED.document, \
                       updated_by_credential_id = EXCLUDED.updated_by_credential_id, updated_at = EXCLUDED.updated_at",
                    vec![
                        env.into(),
                        ctx2.scope.tenant_id.into(),
                        ctx2.scope.application_id.into(),
                        (current + 1).into(),
                        document.into(),
                        ctx2.credential_id.into(),
                        now.into(),
                    ],
                )
                .await?;
                audit::record(
                    txn,
                    AuditEntry {
                        ctx: &ctx2,
                        principal_id: None,
                        action: "POLICY_UPDATED",
                        target_kind: "MESSAGING_POLICY",
                        target_id: env,
                        detail: json!({ "version": current + 1, "digest": resolved.digest() }),
                    },
                    &request_ctx,
                    now,
                )
                .await?;
                outbox::enqueue(
                    txn,
                    NewEvent {
                        event_type: "messaging.policy.updated",
                        aggregate_type: "MESSAGING_POLICY",
                        aggregate_id: env,
                        scope: scope_of(&ctx2),
                        payload: json!({ "version": current + 1, "policy_digest": resolved.digest() }),
                    },
                    &request_ctx,
                    now,
                )
                .await?;
                Ok(())
            })
        })
        .await
        .map_err(Error::from)?;
    policy_view(state, ctx).await
}
