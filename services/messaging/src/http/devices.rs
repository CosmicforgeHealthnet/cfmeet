//! Device and key-directory routes (`/v1/messaging/...`, contract
//! `cfmeet.messaging.http-api`). Every route needs a scope AND a verified acting
//! principal; mutations authenticate afresh (middleware) and resolve the principal afresh.
//!
//! | Route | Scope |
//! |---|---|
//! | `POST /devices/registration-challenges` | `messages.send` |
//! | `POST /devices` | `messages.send` |
//! | `POST /devices/{device_id}/key-packages` | `messages.send` |
//! | `POST /devices/{device_id}/revoke` | `messages.send` |
//! | `GET /principals/{principal_id}/devices` | `messages.read` |
//! | `GET /key-transparency/proofs` | `messages.read` |

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::{ApiError, ErrorCode},
    middleware::{require_acting, CustomerCaller},
};
use crate::{
    devices::{self, PublishRequest, RegisterRequest, RevokeRequest},
    identity::PrincipalStatus,
    state::AppState,
    transparency,
};

pub const MESSAGES_SEND: &str = "messages.send";
pub const MESSAGES_READ: &str = "messages.read";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/devices/registration-challenges", post(create_challenge))
        .route("/devices", post(register))
        .route("/devices/:device_id/key-packages", post(publish))
        .route("/devices/:device_id/revoke", post(revoke))
        .route("/principals/:principal_id/devices", get(directory))
        .route("/key-transparency/proofs", get(proofs))
}

async fn create_challenge(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    let challenge = devices::create_challenge(&state, ctx, &acting).await?;
    Ok((StatusCode::CREATED, Json(challenge)))
}

async fn register(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    let registered = devices::register(&state, ctx, &acting, body).await?;
    let status = if registered.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(devices::device_view(&registered.device))))
}

async fn publish(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(device_id): Path<Uuid>,
    Json(body): Json<PublishRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    let published = devices::publish_key_packages(&state, ctx, &acting, device_id, body).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "deviceId": device_id, "keyPackages": published })),
    ))
}

async fn revoke(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(device_id): Path<Uuid>,
    Json(body): Json<RevokeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(MESSAGES_SEND)?;
    let acting = require_acting(&state, &caller, &headers).await?;
    let device = devices::revoke(&state, ctx, &acting, device_id, body).await?;
    Ok(Json(devices::device_view(&device)))
}

/// The ACTIVE devices of a principal of the caller's own environment. The target must be
/// an ACTIVE messaging principal there (Platform Access, fresh); anything else — another
/// tenant's principal included — is NOT_FOUND. Lookups are rate-limited per credential.
async fn directory(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Path(principal_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(MESSAGES_READ)?;
    require_acting(&state, &caller, &headers).await?;
    devices::directory_lookup_allowed(&state, ctx).await?;
    match state.principals.resolve(&ctx.scope, principal_id).await {
        Ok(status @ PrincipalStatus::Active { .. }) if status.is_messaging_capable() => {}
        Ok(_) => return Err(ApiError::code(ErrorCode::NotFound)),
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            return Err(ApiError::code(ErrorCode::ServiceUnavailable));
        }
    }
    let rows = devices::active_devices(&state, ctx, principal_id).await?;
    let head = transparency::tree_head(&state.db).await?;
    Ok(Json(json!({
        "principalId": principal_id,
        "devices": rows.iter().map(devices::device_view).collect::<Vec<_>>(),
        "treeHead": tree_head_view(&state, &head)?,
    })))
}

/// The signed tree head (Part V G.5). Fails closed without a configured signer.
fn tree_head_view(state: &AppState, head: &transparency::TreeHead) -> Result<Value, ApiError> {
    let Some(signer) = &state.tree_heads else {
        tracing::error!("key-transparency tree-head signer not configured");
        return Err(ApiError::code(ErrorCode::ServiceUnavailable));
    };
    let signed = signer.sign(head);
    Ok(json!({
        "logId": signed.log_id,
        "treeSize": head.tree_size,
        "rootHash": URL_SAFE_NO_PAD.encode(head.root_hash),
        "timestamp": head.timestamp.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        // Exactly what the signature covers (Unix microseconds; JSON-safe).
        "timestampMicros": head.timestamp.timestamp_micros(),
        "hashAlgorithm": "RFC9162_SHA256",
        "signatureAlgorithm": "Ed25519",
        "keyId": signed.key_id,
        "signature": URL_SAFE_NO_PAD.encode(signed.signature),
        "witnessStatus": transparency::WITNESS_STATUS,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProofQuery {
    device_id: Uuid,
    #[serde(default)]
    from_tree_size: Option<u64>,
}

/// Inclusion proofs of a device's log entries against the current tree head and,
/// with `fromTreeSize`, a consistency proof from the tree head the client last saw.
async fn proofs(
    State(state): State<AppState>,
    caller: CustomerCaller,
    headers: HeaderMap,
    Query(q): Query<ProofQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let ctx = caller.require_scope(MESSAGES_READ)?;
    require_acting(&state, &caller, &headers).await?;
    devices::directory_lookup_allowed(&state, ctx).await?;
    // A device of another environment is reported exactly like a missing one.
    if devices::load_device(&state.db, ctx, q.device_id, false)
        .await?
        .is_none()
    {
        return Err(ApiError::code(ErrorCode::NotFound));
    }
    let head = transparency::tree_head(&state.db).await?;
    let leaves =
        transparency::device_leaves(&state.db, ctx.scope.environment_id, q.device_id).await?;
    let mut entries = Vec::with_capacity(leaves.len());
    for leaf in &leaves {
        let proof = transparency::inclusion(&state.db, leaf.leaf_index as u64, &head).await?;
        entries.push(json!({
            "leafIndex": leaf.leaf_index,
            "kind": leaf.entry_kind,
            "leafData": URL_SAFE_NO_PAD.encode(&leaf.leaf_data),
            "inclusionProof": proof.iter().map(|h| URL_SAFE_NO_PAD.encode(h)).collect::<Vec<_>>(),
        }));
    }
    let consistency = match q.from_tree_size {
        None => Value::Null,
        Some(old) => {
            let proof = transparency::consistency(&state.db, old, &head).await?;
            json!({
                "fromTreeSize": old,
                "fromRootHash": URL_SAFE_NO_PAD.encode(transparency::root_at(&state.db, old).await?),
                "proof": proof.iter().map(|h| URL_SAFE_NO_PAD.encode(h)).collect::<Vec<_>>(),
            })
        }
    };
    Ok(Json(json!({
        "deviceId": q.device_id,
        "treeHead": tree_head_view(&state, &head)?,
        "entries": entries,
        "consistency": consistency,
    })))
}
