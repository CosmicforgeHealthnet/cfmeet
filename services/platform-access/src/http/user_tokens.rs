//! User-token verification (ADR 0062; contract `docs/platform/build-internal-contracts.md`
//! §2.2): `platform-user-token.verify` (Messaging, Calling, Meeting Service, API Gateway),
//! behind `PLATFORM_ACCESS_USER_TOKENS_ENABLED` (off: `404 NOT_FOUND` like an unknown
//! route).
//!
//! | Route | Result |
//! |---|---|
//! | `POST /user-tokens/verify` `{token}` | `200` the token's scope, user and EFFECTIVE grants · `401 USER_TOKEN_INVALID / USER_TOKEN_EXPIRED / USER_TOKEN_REVOKED` |
//! | `GET /user-tokens/keys` | `{keys:[{kid, alg, crv, x, status}]}` |

use axum::{extract::State, response::IntoResponse, routing::{get, post}, Json, Router};
use cf_service_auth::InternalPermission;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::{
    error::{AccessError, AccessResult, ErrorCode},
    middleware::InternalCaller,
};
use crate::{
    identity::user_tokens::{self, TokenRejection, VerifyError, MAX_TOKEN_BYTES},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/user-tokens/verify", post(verify))
        .route("/user-tokens/keys", get(keys))
}

fn enabled(state: &AppState) -> AccessResult<&user_tokens::UserTokenKeys> {
    match &state.build.user_token_keys {
        Some(keys) if state.build.verification_enabled() => Ok(keys),
        _ => Err(AccessError::Refused(ErrorCode::NotFound)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyBody {
    token: String,
}

fn rejected(r: TokenRejection) -> AccessError {
    crate::metrics::inc(
        "platform_access_user_token_verifications_total",
        &[("outcome", r.as_str())],
    );
    AccessError::Refused(match r {
        TokenRejection::Invalid => ErrorCode::UserTokenInvalid,
        TokenRejection::Expired => ErrorCode::UserTokenExpired,
        TokenRejection::Revoked => ErrorCode::UserTokenRevoked,
    })
}

fn timestamp(seconds: i64) -> AccessResult<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(seconds, 0).ok_or(AccessError::Refused(ErrorCode::UserTokenInvalid))
}

async fn verify(
    State(state): State<AppState>,
    caller: InternalCaller,
    Json(body): Json<VerifyBody>,
) -> AccessResult<impl IntoResponse> {
    let keys = enabled(&state)?;
    caller.require_permission(InternalPermission::PlatformUserTokenVerify)?;
    if body.token.len() > MAX_TOKEN_BYTES {
        return Err(rejected(TokenRejection::Invalid));
    }
    let verified = match user_tokens::verify_live(
        keys,
        &state.authenticator,
        state.platform.db(),
        body.token.trim(),
        state.platform.clock().now(),
    )
    .await
    {
        Ok(v) => v,
        Err(VerifyError::Rejected(r)) => {
            tracing::info!(caller = caller.service(), outcome = r.as_str(), "user token refused");
            return Err(rejected(r));
        }
        Err(VerifyError::Unavailable(summary)) => {
            tracing::error!(%summary, "user-token verification unavailable");
            return Err(AccessError::Refused(ErrorCode::ServiceUnavailable));
        }
    };
    crate::metrics::inc(
        "platform_access_user_token_verifications_total",
        &[("outcome", "VALID")],
    );
    let c = &verified.claims;
    let u = &verified.user;
    Ok(Json(json!({
        "tokenId": c.token_id,
        "tenantId": c.tenant_id,
        "applicationId": c.application_id,
        "environmentId": c.environment_id,
        "environmentKind": c.environment_kind,
        "credentialId": c.credential_id,
        "serviceAccountId": c.service_account_id,
        "user": {
            "id": u.id,
            "externalId": u.external_reference,
            "displayName": u.display_name,
            "imageUrl": u.image_url,
            "status": u.status,
        },
        "grants": verified.effective,
        "issuedAt": timestamp(c.issued_at)?,
        "expiresAt": timestamp(c.expires_at)?,
    })))
}

async fn keys(
    State(state): State<AppState>,
    caller: InternalCaller,
) -> AccessResult<impl IntoResponse> {
    let keys = enabled(&state)?;
    caller.require_permission(InternalPermission::PlatformUserTokenVerify)?;
    Ok(Json(json!({ "keys": keys.public_keys() })))
}
