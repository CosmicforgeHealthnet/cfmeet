//! The MLS Delivery Service functions of Messaging (RFC 9420 §§ 3, 14; Part V E / F).
//!
//! The backend orders handshake messages; it never holds group secrets:
//!
//! * [`submit_commit`] accepts exactly ONE commit per epoch (the first one wins; a
//!   concurrent commit for the same epoch is `EPOCH_MISMATCH`) after checking the PUBLIC
//!   framing (this conversation's group, a commit, the claimed epoch = the current one).
//!   The commit becomes a message of kind `MLS_COMMIT` in the canonical sequence, so
//!   every member applies commits and application messages in one order. Welcomes for
//!   added devices are stored per device (only ACTIVE devices of ACTIVE members may be
//!   welcomed).
//! * Removal: removing a member or revoking a device records its devices as PENDING
//!   REMOVALS and sets `rekey_required`: until a commit that declares those devices
//!   removed is accepted, application messages are refused, so nothing new is encrypted
//!   to a removed leaf. The server cannot inspect a `PrivateMessage` commit; the
//!   declaration is the committer's attestation, and every remaining member's MLS client
//!   verifies the actual roster when it merges the commit (client SDK duty).
//! * [`claim_key_packages`] hands out the published KeyPackages of ACTIVE members'
//!   devices (single use; `FOR UPDATE SKIP LOCKED`) so a member can add them.
//! * Self-join (RFC 9420 §12.4.3.2): a commit may carry the GroupInfo of the epoch it
//!   creates (PUBLIC material signed by the committer); the latest one is stored and served
//!   to members while its epoch is current. An ACTIVE device of an ACTIVE member that is
//!   not in the tree joins BY ITSELF with an external commit (a `PublicMessage` whose
//!   sender is `new_member_commit`, visible in the clear framing) — no other member has to
//!   be online. The server accepts it only from the acting principal's own ACTIVE device,
//!   never while removals are pending (the joiner's UpdatePath would reach a removed leaf)
//!   and never with Welcomes or removals; every member's client verifies that the joining
//!   leaf is that registered device with its logged key before merging it.

pub mod http;

use cf_messaging_crypto::framing::{self, FramedContent};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::{
        access,
        service::{request_context, rfc3339, scope_of},
    },
    db,
    domain::{
        profile::{OpaqueCiphertext, TransportMetadata},
        SecurityProfile,
    },
    error::{Error, ErrorCode, Result},
    events::outbox::{self, EventScope, NewEvent},
    http::middleware::Acting,
    identity::TrustedContext,
    messages::{
        decode_b64,
        e2ee::{
            self, check_framing, lock_group, require_own_device, NewE2eeMessage,
            MAX_MLS_MESSAGE_BYTES,
        },
        load_by_client_id, record_created, render,
    },
    state::AppState,
};

/// Recomputes `rekey_required` from the pending removals (no-op for MANAGED).
pub async fn refresh_rekey<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    let pending = db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_mls_pending_removals WHERE conversation_id = $1",
        vec![conversation_id.into()],
    )
    .await?
        > 0;
    db::exec(
        db,
        "UPDATE messaging_mls_groups SET rekey_required = $2, \
           rekey_reason = CASE WHEN $2 THEN COALESCE(rekey_reason, 'MEMBER_REMOVED') ELSE NULL END, updated_at = $3 \
         WHERE conversation_id = $1 AND rekey_required IS DISTINCT FROM $2",
        vec![conversation_id.into(), pending.into(), now.into()],
    )
    .await?;
    Ok(pending)
}

/// Pending removals of devices that are NOT leaves of the group's tree at its current
/// epoch are moot (nothing is encrypted to a device outside the tree): they are dropped.
/// The tree is the one of the current epoch's GroupInfo, which the committer of that epoch
/// published with its commit (the same attestation as `removedDeviceIds`; every member's
/// client verifies the tree hash when it joins from it). Without a current GroupInfo
/// nothing is dropped. Returns whether removals are still pending.
pub async fn prune_moot_removals<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool> {
    #[derive(FromQueryResult)]
    struct I {
        group_info: Vec<u8>,
    }
    let info: Option<I> = db::one(
        db,
        "SELECT i.group_info FROM messaging_mls_group_infos i \
           JOIN messaging_mls_groups g ON g.conversation_id = i.conversation_id AND g.mls_epoch = i.mls_epoch \
          WHERE i.conversation_id = $1",
        vec![conversation_id.into()],
    )
    .await?;
    if let Some(tree) = info
        .and_then(|i| framing::inspect_group_info(&i.group_info).ok())
        .and_then(|h| h.member_devices)
    {
        db::exec(
            db,
            "DELETE FROM messaging_mls_pending_removals WHERE conversation_id = $1 AND NOT (device_id = ANY($2))",
            vec![conversation_id.into(), tree.into()],
        )
        .await?;
    }
    refresh_rekey(db, conversation_id, now).await
}

/// Every ACTIVE device of `principal` must leave the group's MLS tree.
pub async fn require_removal_of_principal<C: ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    principal: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    db::exec(
        db,
        "INSERT INTO messaging_mls_pending_removals (conversation_id, device_id, principal_id, reason, created_at) \
         SELECT $1, d.id, d.principal_id, $3, $4 FROM messaging_devices d \
           JOIN messaging_conversations c ON c.id = $1 AND c.environment_id = d.environment_id \
          WHERE d.principal_id = $2 AND d.status = 'ACTIVE' \
         ON CONFLICT DO NOTHING",
        vec![conversation_id.into(), principal.into(), reason.into(), now.into()],
    )
    .await?;
    prune_moot_removals(db, conversation_id, now).await
}

/// A revoked device must leave the MLS tree of every E2EE conversation its principal is
/// an ACTIVE member of (called in the revocation transaction). Returns how many groups
/// now require a rekeying commit.
pub async fn device_revoked<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    principal: Uuid,
    device_id: Uuid,
    now: DateTime<Utc>,
) -> Result<u64> {
    #[derive(FromQueryResult)]
    struct Inserted {
        conversation_id: Uuid,
    }
    let inserted: Vec<Inserted> = db::all(
        db,
        "INSERT INTO messaging_mls_pending_removals (conversation_id, device_id, principal_id, reason, created_at) \
         SELECT m.conversation_id, $3, $2, 'DEVICE_REVOKED', $4 FROM messaging_conversation_members m \
           JOIN messaging_conversations c ON c.id = m.conversation_id \
          WHERE m.environment_id = $1 AND m.principal_id = $2 AND m.state = 'ACTIVE' \
            AND c.security_profile = 'PRIVATE_E2EE' \
         ON CONFLICT DO NOTHING RETURNING conversation_id",
        vec![environment_id.into(), principal.into(), device_id.into(), now.into()],
    )
    .await?;
    db::exec(
        db,
        "UPDATE messaging_mls_groups g SET rekey_required = TRUE, rekey_reason = 'DEVICE_REVOKED', updated_at = $2 \
          WHERE g.rekey_required = FALSE AND EXISTS (SELECT 1 FROM messaging_mls_pending_removals p \
                 WHERE p.conversation_id = g.conversation_id AND p.device_id = $1)",
        vec![device_id.into(), now.into()],
    )
    .await?;
    // A revoked device that never entered a group's tree (e.g. a browser whose storage was
    // lost before it joined) must not block that group until someone commits its removal.
    let mut n = 0;
    for i in &inserted {
        prune_moot_removals(db, i.conversation_id, now).await?;
        n += db::scalar_i64(
            db,
            "SELECT count(*) FROM messaging_mls_pending_removals WHERE conversation_id = $1 AND device_id = $2",
            vec![i.conversation_id.into(), device_id.into()],
        )
        .await? as u64;
    }
    Ok(n)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitRequest {
    pub client_message_id: Uuid,
    pub device_id: Uuid,
    /// The epoch the commit was created in (= the group's current epoch).
    pub epoch: u64,
    pub commit: String,
    #[serde(default)]
    pub welcome: Option<String>,
    #[serde(default)]
    pub welcome_device_ids: Vec<Uuid>,
    /// Devices this commit removes from the tree (the committer's attestation).
    #[serde(default)]
    pub removed_device_ids: Vec<Uuid>,
    /// The GroupInfo of the epoch this commit creates (`MLSMessage` of wire format
    /// `mls_group_info`, signed by the committer, with the ratchet tree and
    /// `external_pub`): lets a member's new device join by itself.
    #[serde(default)]
    pub group_info: Option<String>,
}

#[derive(FromQueryResult)]
struct DeviceOwner {
    id: Uuid,
    principal_id: Uuid,
    status: String,
}

pub async fn submit_commit(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: CommitRequest,
) -> Result<(u16, Value)> {
    let (conversation, _) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    if conversation.profile() != SecurityProfile::PrivateE2ee {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "NOT_AN_E2EE_CONVERSATION",
        ));
    }
    access::require_accepting(&conversation, state.now())?;
    if request.welcome_device_ids.len() > 1000 || request.removed_device_ids.len() > 1000 {
        return Err(Error::validation(
            "at most 1000 welcomed / removed devices per commit",
        ));
    }
    if request.welcome.is_some() == request.welcome_device_ids.is_empty() {
        return Err(Error::validation(
            "welcome and welcomeDeviceIds go together",
        ));
    }
    let commit = OpaqueCiphertext::new(decode_b64(
        "commit",
        &request.commit,
        MAX_MLS_MESSAGE_BYTES,
    )?)
    .map_err(|_| Error::reason(ErrorCode::ContentRejected, "SIZE"))?;
    let welcome = match &request.welcome {
        Some(w) => {
            let bytes = decode_b64("welcome", w, MAX_MLS_MESSAGE_BYTES)?;
            if !framing::is_welcome(&bytes) {
                return Err(Error::reason(ErrorCode::ContentRejected, "NOT_A_WELCOME"));
            }
            Some(bytes)
        }
        None => None,
    };
    require_own_device(&state.db, ctx, acting.principal_id, request.device_id).await?;
    let framed = check_framing(
        &conversation,
        commit.as_bytes(),
        FramedContent::Commit,
        request.epoch,
    )?;
    // An external commit: the acting principal's own device joins by itself. It can
    // neither welcome nor remove anyone (RFC 9420 §12.4.3.2: only its own stale leaf).
    let external = framed.new_member_commit;
    if external && (welcome.is_some() || !request.removed_device_ids.is_empty()) {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "EXTERNAL_COMMIT_CANNOT_WELCOME_OR_REMOVE",
        ));
    }
    let group_info = match &request.group_info {
        Some(g) => {
            let bytes = decode_b64("groupInfo", g, MAX_MLS_MESSAGE_BYTES)?;
            let header = framing::inspect_group_info(&bytes)
                .map_err(|_| Error::reason(ErrorCode::ContentRejected, "NOT_A_GROUP_INFO"))?;
            if Some(header.group_id.as_slice()) != conversation.mls_group_id.as_deref()
                || header.epoch != request.epoch + 1
            {
                return Err(Error::reason(
                    ErrorCode::ContentRejected,
                    "GROUP_INFO_MISMATCH",
                ));
            }
            Some(bytes)
        }
        None => None,
    };
    // Welcomed devices: ACTIVE devices of ACTIVE members of this conversation.
    if !request.welcome_device_ids.is_empty() {
        let owners: Vec<DeviceOwner> = db::all(
            &state.db,
            "SELECT id, principal_id, status FROM messaging_devices WHERE id = ANY($1) AND environment_id = $2",
            vec![request.welcome_device_ids.clone().into(), ctx.scope.environment_id.into()],
        )
        .await?;
        for id in &request.welcome_device_ids {
            let o = owners.iter().find(|o| o.id == *id).ok_or_else(|| {
                Error::reason(
                    ErrorCode::PrincipalNotAuthorized,
                    "WELCOME_TARGET_NOT_A_MEMBER",
                )
            })?;
            let member =
                access::membership(&state.db, conversation.id, o.principal_id, false).await?;
            if o.status != "ACTIVE" || !member.is_some_and(|m| m.is_active()) {
                return Err(Error::reason(
                    ErrorCode::PrincipalNotAuthorized,
                    "WELCOME_TARGET_NOT_A_MEMBER",
                ));
            }
        }
    }
    let metadata = TransportMetadata {
        client_message_id: request.client_message_id,
        sender_device_id: request.device_id,
        epoch: request.epoch,
    };
    metadata
        .validate()
        .map_err(|_| Error::validation("invalid transport metadata"))?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let acting = acting.clone();
    let request_ctx = request_context();
    let (status, row) = state
        .db
        .transaction::<_, (u16, crate::messages::MessageRow), Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                crate::messages::lock_sender(txn, &conversation, acting.principal_id, now, false).await?;
                crate::communications::require_send(txn,&st,&ctx2,&conversation,acting.principal_id).await?;
                let group = lock_group(txn, conversation.id).await?;
                if let Some(existing) =
                    load_by_client_id(txn, conversation.id, acting.principal_id, request.client_message_id).await?
                {
                    e2ee::replayed(&existing, &commit)?;
                    return Ok((200, existing));
                }
                if group.mls_epoch as u64 != request.epoch {
                    return Err(Error::code(ErrorCode::EpochMismatch));
                }
                // An external join is built from the current GroupInfo: removals of devices
                // that are not in its tree are moot and dropped first.
                if external {
                    prune_moot_removals(txn, conversation.id, now).await?;
                }
                // A commit while removals are pending must remove every pending device.
                #[derive(FromQueryResult)]
                struct P {
                    device_id: Uuid,
                }
                let pending: Vec<P> = db::all(
                    txn,
                    "SELECT device_id FROM messaging_mls_pending_removals WHERE conversation_id = $1",
                    vec![conversation.id.into()],
                )
                .await?;
                if pending.iter().any(|p| !request.removed_device_ids.contains(&p.device_id)) {
                    return Err(Error::reason(
                        ErrorCode::MlsRekeyRequired,
                        if external { "REMOVALS_PENDING_BEFORE_JOIN" } else { "COMMIT_MISSING_REMOVALS" },
                    ));
                }
                let row = e2ee::insert(
                    txn,
                    NewE2eeMessage {
                        conversation: &conversation,
                        ctx: &ctx2,
                        sender: acting.principal_id,
                        kind: "MLS_COMMIT",
                        target: None,
                        metadata,
                        ciphertext: &commit,
                        attachment_count: 0,
                    },
                    now,
                )
                .await?;
                db::exec(
                    txn,
                    "UPDATE messaging_mls_groups SET mls_epoch = mls_epoch + 1, commits_accepted = commits_accepted + 1, \
                       updated_at = $2 WHERE conversation_id = $1",
                    vec![conversation.id.into(), now.into()],
                )
                .await?;
                if !request.removed_device_ids.is_empty() {
                    db::exec(
                        txn,
                        "DELETE FROM messaging_mls_pending_removals WHERE conversation_id = $1 AND device_id = ANY($2)",
                        vec![conversation.id.into(), request.removed_device_ids.clone().into()],
                    )
                    .await?;
                }
                refresh_rekey(txn, conversation.id, now).await?;
                if let Some(info) = &group_info {
                    db::exec(
                        txn,
                        "INSERT INTO messaging_mls_group_infos (conversation_id, mls_epoch, group_info, publisher_device_id, updated_at) \
                         VALUES ($1, $2, $3, $4, $5) \
                         ON CONFLICT (conversation_id) DO UPDATE SET mls_epoch = EXCLUDED.mls_epoch, \
                           group_info = EXCLUDED.group_info, publisher_device_id = EXCLUDED.publisher_device_id, \
                           updated_at = EXCLUDED.updated_at",
                        vec![
                            conversation.id.into(),
                            (request.epoch as i64 + 1).into(),
                            info.clone().into(),
                            request.device_id.into(),
                            now.into(),
                        ],
                    )
                    .await?;
                }
                if external {
                    crate::metrics::inc("messaging_mls_external_joins_total", &[]);
                }
                record_created(txn, &ctx2, &row, &request_ctx, now).await?;
                if let Some(w) = &welcome {
                    for device in &request.welcome_device_ids {
                        let welcome_id = Uuid::new_v4();
                        db::exec(
                            txn,
                            "INSERT INTO messaging_mls_welcomes (id, conversation_id, device_id, commit_message_id, mls_epoch, \
                               welcome, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                            vec![
                                welcome_id.into(),
                                conversation.id.into(),
                                (*device).into(),
                                row.id.into(),
                                (request.epoch as i64 + 1).into(),
                                w.clone().into(),
                                now.into(),
                            ],
                        )
                        .await?;
                        outbox::enqueue(
                            txn,
                            NewEvent {
                                event_type: "messaging.mls.welcome-available",
                                aggregate_type: "DEVICE",
                                aggregate_id: *device,
                                scope: scope_of(&ctx2),
                                payload: json!({
                                    "welcome_id": welcome_id,
                                    "device_id": device,
                                    "conversation_id": conversation.id,
                                }),
                            },
                            &request_ctx,
                            now,
                        )
                        .await?;
                    }
                }
                crate::metrics::inc("messaging_mls_commits_accepted_total", &[]);
                Ok((201, row))
            })
        })
        .await
        .map_err(Error::from)?;
    let mut view = render(state, &row, false).await;
    view["mlsEpoch"] = json!(row.mls_epoch.unwrap_or(0) + 1);
    Ok((status, view))
}

/// The group's Delivery Service state as members see it.
pub async fn group_state(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
) -> Result<Value> {
    let (conversation, _) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    if conversation.profile() != SecurityProfile::PrivateE2ee {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "NOT_AN_E2EE_CONVERSATION",
        ));
    }
    #[derive(FromQueryResult)]
    struct G {
        mls_epoch: i64,
        rekey_required: bool,
        rekey_reason: Option<String>,
        commits_accepted: i64,
    }
    let g: G = db::one(
        &state.db,
        "SELECT mls_epoch, rekey_required, rekey_reason, commits_accepted FROM messaging_mls_groups WHERE conversation_id = $1",
        vec![conversation_id.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    #[derive(FromQueryResult)]
    struct P {
        device_id: Uuid,
        principal_id: Uuid,
        reason: String,
    }
    let pending: Vec<P> = db::all(
        &state.db,
        "SELECT device_id, principal_id, reason FROM messaging_mls_pending_removals WHERE conversation_id = $1 ORDER BY created_at",
        vec![conversation_id.into()],
    )
    .await?;
    #[derive(FromQueryResult)]
    struct I {
        mls_epoch: i64,
        group_info: Vec<u8>,
    }
    // Only the CURRENT epoch's GroupInfo is useful (an external commit must be created in
    // the current epoch); an older one is never served.
    let info: Option<I> = db::one(
        &state.db,
        "SELECT mls_epoch, group_info FROM messaging_mls_group_infos WHERE conversation_id = $1 AND mls_epoch = $2",
        vec![conversation_id.into(), g.mls_epoch.into()],
    )
    .await?;
    Ok(json!({
        "conversationId": conversation_id,
        "mlsEpoch": g.mls_epoch,
        "rekeyRequired": g.rekey_required,
        "rekeyReason": g.rekey_reason,
        "commitsAccepted": g.commits_accepted,
        "pendingRemovals": pending.iter().map(|p| json!({
            "deviceId": p.device_id, "principalId": p.principal_id, "reason": p.reason,
        })).collect::<Vec<_>>(),
        "groupInfo": info.as_ref().map(|i| crate::messages::b64(&i.group_info)),
        "groupInfoEpoch": info.as_ref().map(|i| i.mls_epoch),
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimRequest {
    pub principal_ids: Vec<Uuid>,
    /// Only these devices of the listed principals (a member adding a principal's NEW
    /// device leaves the KeyPackages of devices already in the tree untouched).
    #[serde(default)]
    pub device_ids: Option<Vec<Uuid>>,
}

/// Claims one KeyPackage of every ACTIVE device of the given ACTIVE members (so the
/// acting member can add them to the MLS group). Single use.
pub async fn claim_key_packages(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    conversation_id: Uuid,
    request: ClaimRequest,
) -> Result<Value> {
    if request.principal_ids.is_empty() || request.principal_ids.len() > 100 {
        return Err(Error::validation("principalIds must hold 1-100 principals"));
    }
    if request
        .device_ids
        .as_ref()
        .is_some_and(|d| d.is_empty() || d.len() > 1000)
    {
        return Err(Error::validation("deviceIds must hold 1-1000 devices"));
    }
    let (conversation, _) =
        access::as_member(&state.db, ctx, conversation_id, acting.principal_id).await?;
    if conversation.profile() != SecurityProfile::PrivateE2ee {
        return Err(Error::reason(
            ErrorCode::ContentRejected,
            "NOT_AN_E2EE_CONVERSATION",
        ));
    }
    crate::devices::directory_lookup_allowed(state, ctx, acting.principal_id).await?;
    let st = state.clone();
    let ctx2 = ctx.clone();
    let claimer = acting.principal_id;
    state
        .db
        .transaction::<_, Value, Error>(move |txn| {
            Box::pin(async move {
                let now = st.now();
                let reservation = Uuid::new_v4();
                let mut out = Vec::new();
                for p in &request.principal_ids {
                    if !access::membership(txn, conversation.id, *p, false)
                        .await?
                        .is_some_and(|m| m.is_active())
                    {
                        return Err(Error::reason(
                            ErrorCode::PrincipalNotAuthorized,
                            "NOT_A_MEMBER",
                        ));
                    }
                    // Appendix K: never hand a blocked principal's KeyPackages to the
                    // principal on the other side of the block.
                    if *p != claimer
                        && crate::safety::blocks::blocked_between(
                            txn,
                            ctx2.scope.environment_id,
                            claimer,
                            *p,
                        )
                        .await?
                    {
                        return Err(Error::reason(
                            ErrorCode::PrincipalNotAuthorized,
                            "NOT_A_MEMBER",
                        ));
                    }
                    for k in crate::devices::claim_key_packages_of(
                        txn,
                        ctx2.scope.environment_id,
                        *p,
                        request.device_ids.as_deref(),
                        reservation,
                        now,
                        chrono::Duration::hours(1),
                    )
                    .await?
                    {
                        out.push(json!({
                            "principalId": p,
                            "deviceId": k.device_id,
                            "keyPackage": crate::messages::b64(&k.encoded),
                        }));
                    }
                }
                Ok(json!({ "reservationId": reservation, "keyPackages": out }))
            })
        })
        .await
        .map_err(Error::from)
}

#[derive(FromQueryResult)]
struct WelcomeRow {
    id: Uuid,
    conversation_id: Uuid,
    mls_epoch: i64,
    welcome: Vec<u8>,
    created_at: DateTime<Utc>,
}

/// Pending Welcomes of one of the acting principal's devices (only while the principal
/// is still an ACTIVE member of the conversation).
pub async fn pending_welcomes(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
) -> Result<Value> {
    require_own_device(&state.db, ctx, acting.principal_id, device_id).await?;
    let rows: Vec<WelcomeRow> = db::all(
        &state.db,
        "SELECT w.id, w.conversation_id, w.mls_epoch, w.welcome, w.created_at FROM messaging_mls_welcomes w \
           JOIN messaging_conversation_members m ON m.conversation_id = w.conversation_id AND m.principal_id = $2 \
          WHERE w.device_id = $1 AND w.consumed_at IS NULL AND m.state = 'ACTIVE' ORDER BY w.created_at LIMIT 100",
        vec![device_id.into(), acting.principal_id.into()],
    )
    .await?;
    Ok(json!({
        "welcomes": rows.iter().map(|w| json!({
            "welcomeId": w.id,
            "conversationId": w.conversation_id,
            "mlsEpoch": w.mls_epoch,
            "welcome": crate::messages::b64(&w.welcome),
            "createdAt": rfc3339(w.created_at),
        })).collect::<Vec<_>>(),
    }))
}

pub async fn consume_welcome(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
    welcome_id: Uuid,
) -> Result<Value> {
    require_own_device(&state.db, ctx, acting.principal_id, device_id).await?;
    let n = db::exec(
        &state.db,
        "UPDATE messaging_mls_welcomes SET consumed_at = COALESCE(consumed_at, $3) WHERE id = $1 AND device_id = $2",
        vec![welcome_id.into(), device_id.into(), state.now().into()],
    )
    .await?;
    if n == 0 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    Ok(json!({ "welcomeId": welcome_id, "consumed": true }))
}

/// Scope of an event of a device (helper for callers outside a request).
pub fn device_scope(tenant_id: Uuid, application_id: Uuid, environment_id: Uuid) -> EventScope {
    EventScope {
        tenant_id,
        application_id,
        environment_id,
    }
}
