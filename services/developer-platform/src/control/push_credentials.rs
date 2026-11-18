//! ADR 0063 — partner push credentials (build contract §3, Notification Service N-P1).
//!
//! OWNER / ADMIN upload an environment's APNs (`.p8` token key; VoIP or not) or FCM
//! (service-account JSON) credential from the console. The secret is sealed to the
//! Notification Service's public key the moment it is parsed
//! ([`crate::push_providers::seal_secret`]), forwarded to the Notification Service's
//! provider vault over signed service auth (`notifications-push-provider.manage`) and
//! dropped: it is never stored, logged or returned. This service keeps metadata only
//! (`devplat_push_providers`): provider, bundle / project, team and key ids, APNs host,
//! the SHA-256 fingerprint of the secret, the verification state and timestamps.
//!
//! * One credential per (environment, provider) — `APNS`, `APNS_VOIP` (`voip: true`) or
//!   `FCM`; uploading again replaces it (new `credentialRef`).
//! * SANDBOX and TEST environments may only use `apnsHost: SANDBOX` (422
//!   `POLICY_VIOLATION`); a PRODUCTION environment needs its production access LIVE with
//!   the `PUSH` capability approved (403 `PRODUCTION_ACCESS_REQUIRED`).
//! * Human console only (no machine scope); OWNER / ADMIN for reads and changes.
//!
//! Flag `DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED` (off: 404). Without the
//! Notification Service or its sealing key: 503 `CAPABILITY_UNAVAILABLE`.

use std::fmt;

use axum::http::StatusCode;
use chrono::Utc;
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    activity::{self, Event},
    authorize,
    service_accounts::{environment, EnvironmentRef},
    stmt, Caller,
};
use crate::{
    domain::rbac::Action,
    http::error::{ApiError, ErrorCode},
    push_providers::{seal_secret, PushProviders, PushReply, PushScope},
    state::AppState,
};

/// Largest `.p8` key accepted (bytes).
const MAX_P8_BYTES: usize = 8 * 1024;
/// Largest FCM service-account JSON accepted (bytes).
const MAX_SERVICE_ACCOUNT_BYTES: usize = 16 * 1024;

fn enabled(state: &AppState) -> Result<(), ApiError> {
    state
        .build
        .push_credentials
        .then_some(())
        .ok_or(ApiError(ErrorCode::NotFound))
}

fn configured(state: &AppState) -> Result<&PushProviders, ApiError> {
    state
        .push
        .as_ref()
        .ok_or(ApiError(ErrorCode::CapabilityUnavailable))
}

/// A parsed upload. `Debug` never shows the secret.
pub enum Upload {
    Apns {
        team_id: String,
        key_id: String,
        bundle_id: String,
        voip: bool,
        apns_host: String,
        p8: String,
    },
    Fcm {
        project_id: String,
        service_account_json: String,
    },
}

impl fmt::Debug for Upload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Upload::Apns {
                team_id,
                key_id,
                bundle_id,
                voip,
                apns_host,
                ..
            } => f
                .debug_struct("Apns")
                .field("team_id", team_id)
                .field("key_id", key_id)
                .field("bundle_id", bundle_id)
                .field("voip", voip)
                .field("apns_host", apns_host)
                .field("p8", &"[REDACTED]")
                .finish(),
            Upload::Fcm { project_id, .. } => f
                .debug_struct("Fcm")
                .field("project_id", project_id)
                .field("service_account_json", &"[REDACTED]")
                .finish(),
        }
    }
}

fn apple_id(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn bundle_id(value: &str) -> bool {
    (3..=155).contains(&value.len())
        && value.contains('.')
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

/// A Google Cloud project id: 6–30 of `[a-z0-9-]`, starting with a letter, not ending
/// with `-`.
fn gcp_project_id(value: &str) -> bool {
    let b = value.as_bytes();
    (6..=30).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[b.len() - 1] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn p8_key(value: &str) -> bool {
    let v = value.trim();
    v.len() <= MAX_P8_BYTES
        && v.starts_with("-----BEGIN PRIVATE KEY-----")
        && v.ends_with("-----END PRIVATE KEY-----")
        && v.len() > 60
}

fn service_account(value: &str, project_id: &str) -> bool {
    if value.len() > MAX_SERVICE_ACCOUNT_BYTES {
        return false;
    }
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(value) else {
        return false;
    };
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    s("type") == Some("service_account")
        && s("private_key").is_some_and(|k| k.contains("PRIVATE KEY"))
        && s("client_email").is_some_and(|e| e.contains('@'))
        && s("project_id").is_none_or(|p| p == project_id)
}

impl Upload {
    /// Parses the upload body (exactly the fields of its provider).
    pub fn parse(body: &Value) -> Result<Self, ApiError> {
        let invalid = || ApiError(ErrorCode::ValidationError);
        let map: &Map<String, Value> = body.as_object().ok_or_else(invalid)?;
        let s = |k: &str| map.get(k).and_then(Value::as_str);
        let only = |keys: &[&str]| map.keys().all(|k| keys.contains(&k.as_str()));
        match s("provider") {
            Some("APNS") => {
                if !only(&[
                    "provider", "teamId", "keyId", "bundleId", "voip", "apnsHost", "p8",
                ]) {
                    return Err(invalid());
                }
                let team_id = s("teamId").filter(|v| apple_id(v)).ok_or_else(invalid)?;
                let key_id = s("keyId").filter(|v| apple_id(v)).ok_or_else(invalid)?;
                let bundle = s("bundleId").filter(|v| bundle_id(v)).ok_or_else(invalid)?;
                let voip = map
                    .get("voip")
                    .and_then(Value::as_bool)
                    .ok_or_else(invalid)?;
                let host = s("apnsHost")
                    .filter(|h| matches!(*h, "SANDBOX" | "PRODUCTION"))
                    .ok_or_else(invalid)?;
                let p8 = s("p8").filter(|k| p8_key(k)).ok_or_else(invalid)?;
                Ok(Upload::Apns {
                    team_id: team_id.into(),
                    key_id: key_id.into(),
                    bundle_id: bundle.into(),
                    voip,
                    apns_host: host.into(),
                    p8: p8.trim().to_string(),
                })
            }
            Some("FCM") => {
                if !only(&["provider", "projectId", "serviceAccountJson"]) {
                    return Err(invalid());
                }
                let project = s("projectId")
                    .filter(|v| gcp_project_id(v))
                    .ok_or_else(invalid)?;
                let json = s("serviceAccountJson")
                    .filter(|j| service_account(j, project))
                    .ok_or_else(invalid)?;
                Ok(Upload::Fcm {
                    project_id: project.into(),
                    service_account_json: json.to_string(),
                })
            }
            _ => Err(invalid()),
        }
    }

    /// The Notification Service provider key (`APNS`, `APNS_VOIP`, `FCM`).
    pub fn provider(&self) -> &'static str {
        match self {
            Upload::Apns { voip: true, .. } => "APNS_VOIP",
            Upload::Apns { voip: false, .. } => "APNS",
            Upload::Fcm { .. } => "FCM",
        }
    }

    fn secret(&self) -> &[u8] {
        match self {
            Upload::Apns { p8, .. } => p8.as_bytes(),
            Upload::Fcm {
                service_account_json,
                ..
            } => service_account_json.as_bytes(),
        }
    }

    /// `sha256(secret)`, lowercase hex.
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(self.secret()))
    }

    /// The N-P1 `metadata` (never the secret).
    fn metadata(&self) -> Value {
        match self {
            Upload::Apns {
                team_id,
                key_id,
                bundle_id,
                apns_host,
                ..
            } => json!({
                "teamId": team_id, "keyId": key_id, "bundleId": bundle_id, "apnsHost": apns_host,
            }),
            Upload::Fcm { project_id, .. } => json!({ "projectId": project_id }),
        }
    }
}

const VIEW: &str = "jsonb_build_object( \
    'credentialRef', id, 'environmentId', environment_id, \
    'provider', CASE WHEN provider = 'FCM' THEN 'FCM' ELSE 'APNS' END, \
    'voip', provider = 'APNS_VOIP', 'teamId', apns_team_id, 'keyId', apns_key_id, \
    'bundleId', apns_bundle_id, 'apnsHost', apns_host, 'projectId', fcm_project_id, \
    'fingerprint', fingerprint, 'status', verification, 'lastVerifiedAt', last_verified_at, \
    'lastError', last_error_code, 'createdAt', created_at, 'updatedAt', updated_at)";

async fn row_view<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    credential_ref: Uuid,
) -> Result<Option<(String, Value)>, ApiError> {
    let row = db
        .query_one(stmt(
            &format!(
                "SELECT provider, {VIEW} AS v FROM devplat_push_providers \
                 WHERE environment_id = $1 AND id = $2"
            ),
            vec![environment_id.into(), credential_ref.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => Some((r.try_get("", "provider")?, r.try_get("", "v")?)),
        None => None,
    })
}

fn scope(env: &EnvironmentRef, environment_id: Uuid) -> PushScope {
    PushScope {
        tenant_id: env.tenant_id,
        application_id: env.project_id,
        environment_id,
    }
}

fn vault_refusal(status: u16, code: &str) -> ApiError {
    tracing::info!(status, %code, "the notification service refused a push provider operation");
    ApiError(match (status, code) {
        (_, "VALIDATION_ERROR") | (400 | 422, _) => ErrorCode::ValidationError,
        (_, "POLICY_VIOLATION") => ErrorCode::PolicyViolation,
        (404, _) => ErrorCode::NotFound,
        (409, _) => ErrorCode::InvalidState,
        _ => ErrorCode::ServiceUnavailable,
    })
}

fn unavailable(e: crate::push_providers::PushUnavailable) -> ApiError {
    tracing::warn!(error = %e, "notification service unavailable");
    ApiError(ErrorCode::ServiceUnavailable)
}

/// A bounded verification error code (`[A-Z0-9_]{1,64}`), else a generic one.
fn error_code(v: Option<&Value>) -> Option<String> {
    let raw = match v? {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("code")?.as_str()?.to_string(),
        _ => return Some("VERIFICATION_FAILED".into()),
    };
    let ok = (1..=64).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    Some(if ok { raw } else { "VERIFICATION_FAILED".into() })
}

fn verification(v: &Value) -> &'static str {
    match v.get("status").and_then(Value::as_str) {
        Some("VERIFIED") => "VERIFIED",
        Some("FAILED") => "FAILED",
        _ => "UNVERIFIED",
    }
}

/// `GET /v1/environments/{environmentId}/push-credentials` (metadata only).
pub async fn list(state: &AppState, caller: &Caller, environment_id: Uuid) -> Result<Value, ApiError> {
    enabled(state)?;
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::PushCredentialsRead).await?;
    let rows = state
        .db
        .query_all(stmt(
            &format!(
                "SELECT {VIEW} AS v FROM devplat_push_providers WHERE environment_id = $1 \
                 ORDER BY created_at, id"
            ),
            vec![environment_id.into()],
        ))
        .await?;
    let items = rows
        .iter()
        .map(|r| r.try_get::<Value>("", "v"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "items": items, "nextCursor": null }))
}

/// `POST /v1/environments/{environmentId}/push-credentials` — seals and forwards the
/// secret, stores the metadata; replaces the environment's credential of that provider.
pub async fn create(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    body: &Value,
) -> Result<(StatusCode, Value), ApiError> {
    enabled(state)?;
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::PushCredentialsManage).await?;
    let upload = Upload::parse(body)?;
    if let Upload::Apns { apns_host, .. } = &upload {
        if env.kind != "PRODUCTION" && apns_host != "SANDBOX" {
            return Err(ApiError(ErrorCode::PolicyViolation));
        }
    }
    if env.kind == "PRODUCTION"
        && !(env.production_access_state == "LIVE"
            && super::production::production_capability_live(&state.db, env.project_id, "PUSH")
                .await?)
    {
        return Err(ApiError(ErrorCode::ProductionAccessRequired));
    }
    if env.status != "ACTIVE" {
        return Err(ApiError(ErrorCode::InvalidState));
    }
    let push = configured(state)?;
    let provider = upload.provider();
    let scope = scope(&env, environment_id);
    let fingerprint = upload.fingerprint();
    let sealed = seal_secret(&push.sealing_key, &scope, provider, upload.secret()).map_err(|_| {
        tracing::warn!("a push secret could not be sealed");
        ApiError(ErrorCode::ValidationError)
    })?;
    let request = json!({
        "sealed": sealed, "metadata": upload.metadata(), "fingerprint": fingerprint,
    });
    let reply = push
        .port
        .put(&scope, provider, &request, &caller.request.correlation_id)
        .await
        .map_err(unavailable)?;
    // The secret is never needed again.
    drop(request);
    let stored = match reply {
        PushReply::Ok(v) => v,
        PushReply::Refused { status, code } => return Err(vault_refusal(status, &code)),
    };
    let id = Uuid::new_v4();
    let now = Utc::now();
    let (team, key, bundle, host, project) = match &upload {
        Upload::Apns {
            team_id,
            key_id,
            bundle_id,
            apns_host,
            ..
        } => (
            Some(team_id.clone()),
            Some(key_id.clone()),
            Some(bundle_id.clone()),
            Some(apns_host.clone()),
            None,
        ),
        Upload::Fcm { project_id, .. } => (None, None, None, None, Some(project_id.clone())),
    };
    drop(upload);
    let txn = state.db.begin().await?;
    let replaced = txn
        .execute(stmt(
            "DELETE FROM devplat_push_providers WHERE environment_id = $1 AND provider = $2",
            vec![environment_id.into(), provider.into()],
        ))
        .await?
        .rows_affected()
        > 0;
    txn.execute(stmt(
        "INSERT INTO devplat_push_providers (id, tenant_id, application_id, environment_id, \
             environment_kind, provider, apns_team_id, apns_key_id, apns_bundle_id, apns_host, \
             fcm_project_id, fingerprint, verification, created_by, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)",
        vec![
            id.into(),
            env.tenant_id.into(),
            env.project_id.into(),
            environment_id.into(),
            env.kind.clone().into(),
            provider.into(),
            team.into(),
            key.into(),
            bundle.into(),
            host.into(),
            project.into(),
            fingerprint.clone().into(),
            verification(&stored).into(),
            caller.actor_ref().into(),
            now.into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: if replaced {
                "push_credential.replaced"
            } else {
                "push_credential.uploaded"
            },
            target_type: "PUSH_CREDENTIAL",
            target_id: id.to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id,
                "provider": provider,
            }),
        },
    )
    .await?;
    let (_, v) = row_view(&txn, environment_id, id)
        .await?
        .ok_or(ApiError(ErrorCode::InternalError))?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, v))
}

/// `DELETE /v1/environments/{environmentId}/push-credentials/{credentialRef}`.
pub async fn delete(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    credential_ref: Uuid,
) -> Result<(), ApiError> {
    enabled(state)?;
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::PushCredentialsManage).await?;
    let (provider, _) = row_view(&state.db, environment_id, credential_ref)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let push = configured(state)?;
    match push
        .port
        .delete(
            &scope(&env, environment_id),
            &provider,
            &caller.request.correlation_id,
        )
        .await
        .map_err(unavailable)?
    {
        // Already gone at the vault: the metadata goes too.
        PushReply::Ok(_) | PushReply::Refused { status: 404, .. } => {}
        PushReply::Refused { status, code } => return Err(vault_refusal(status, &code)),
    }
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "DELETE FROM devplat_push_providers WHERE environment_id = $1 AND id = $2",
        vec![environment_id.into(), credential_ref.into()],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: "push_credential.deleted",
            target_type: "PUSH_CREDENTIAL",
            target_id: credential_ref.to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id,
                "provider": provider,
            }),
        },
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

/// `POST /v1/environments/{environmentId}/push-credentials/{credentialRef}/verify` — the
/// Notification Service builds an APNs JWT / fetches an FCM token with the stored key.
pub async fn verify(
    state: &AppState,
    caller: &Caller,
    environment_id: Uuid,
    credential_ref: Uuid,
) -> Result<Value, ApiError> {
    enabled(state)?;
    let env = environment(&state.db, environment_id).await?;
    authorize(state, caller, env.tenant_id, Action::PushCredentialsManage).await?;
    let (provider, _) = row_view(&state.db, environment_id, credential_ref)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    let push = configured(state)?;
    let reply = match push
        .port
        .verify(
            &scope(&env, environment_id),
            &provider,
            &caller.request.correlation_id,
        )
        .await
        .map_err(unavailable)?
    {
        PushReply::Ok(v) => v,
        PushReply::Refused { status, code } => return Err(vault_refusal(status, &code)),
    };
    let status = verification(&reply);
    let last_error = if status == "VERIFIED" {
        None
    } else {
        error_code(reply.get("lastError")).or(Some("VERIFICATION_FAILED".into()))
    };
    let now = Utc::now();
    let txn = state.db.begin().await?;
    txn.execute(stmt(
        "UPDATE devplat_push_providers SET verification = $3, last_verified_at = $4, \
             last_error_code = $5, updated_at = $4 WHERE environment_id = $1 AND id = $2",
        vec![
            environment_id.into(),
            credential_ref.into(),
            status.into(),
            now.into(),
            last_error.into(),
        ],
    ))
    .await?;
    activity::record(
        &txn,
        caller,
        Event {
            tenant_id: env.tenant_id,
            action: "push_credential.verified",
            target_type: "PUSH_CREDENTIAL",
            target_id: credential_ref.to_string(),
            summary: json!({
                "projectId": env.project_id, "environmentId": environment_id,
                "provider": provider, "outcome": status,
            }),
        },
    )
    .await?;
    let (_, v) = row_view(&txn, environment_id, credential_ref)
        .await?
        .ok_or(ApiError(ErrorCode::NotFound))?;
    txn.commit().await?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const P8: &str = "-----BEGIN PRIVATE KEY-----\nMIGTAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBHkwdwIBAQQgSECRETSECRET\n-----END PRIVATE KEY-----";

    fn apns(host: &str) -> Value {
        json!({ "provider": "APNS", "teamId": "ABCDE12345", "keyId": "KEY1234567",
            "bundleId": "com.example.rider", "voip": true, "apnsHost": host, "p8": P8 })
    }

    #[test]
    fn uploads_are_parsed_strictly_and_never_debug_the_secret() {
        let u = Upload::parse(&apns("SANDBOX")).unwrap();
        assert_eq!(u.provider(), "APNS_VOIP");
        assert!(!format!("{u:?}").contains("SECRET"));
        assert_eq!(u.fingerprint(), hex::encode(Sha256::digest(P8.as_bytes())));
        assert!(!u.metadata().to_string().contains("SECRET"));
        let mut extra = apns("SANDBOX");
        extra["projectId"] = json!("x");
        assert!(Upload::parse(&extra).is_err());
        for (k, v) in [
            ("teamId", json!("abc")),
            ("keyId", json!("lower12345")),
            ("bundleId", json!("nodots")),
            ("voip", json!("yes")),
            ("apnsHost", json!("DEV")),
            ("p8", json!("not a key")),
        ] {
            let mut b = apns("SANDBOX");
            b[k] = v;
            assert!(Upload::parse(&b).is_err(), "{k}");
        }
        let sa = json!({ "type": "service_account", "project_id": "rider-app-1",
            "private_key": "-----BEGIN PRIVATE KEY-----\nX\n-----END PRIVATE KEY-----\n",
            "client_email": "push@rider-app-1.iam.gserviceaccount.com" })
        .to_string();
        let fcm = Upload::parse(&json!({ "provider": "FCM", "projectId": "rider-app-1",
            "serviceAccountJson": sa }))
        .unwrap();
        assert_eq!(fcm.provider(), "FCM");
        assert!(!format!("{fcm:?}").contains("PRIVATE"));
        assert!(Upload::parse(&json!({ "provider": "FCM", "projectId": "other-app-1",
            "serviceAccountJson": sa }))
        .is_err());
        assert!(Upload::parse(&json!({ "provider": "WNS" })).is_err());
    }

    #[test]
    fn verification_answers_are_bounded() {
        assert_eq!(verification(&json!({ "status": "VERIFIED" })), "VERIFIED");
        assert_eq!(verification(&json!({ "status": "odd" })), "UNVERIFIED");
        assert_eq!(
            error_code(Some(&json!("APNS_BAD_KEY"))).as_deref(),
            Some("APNS_BAD_KEY")
        );
        assert_eq!(
            error_code(Some(&json!({ "code": "FCM_UNAUTHORIZED" }))).as_deref(),
            Some("FCM_UNAUTHORIZED")
        );
        assert_eq!(
            error_code(Some(&json!("contains secret text"))).as_deref(),
            Some("VERIFICATION_FAILED")
        );
        assert_eq!(error_code(None), None);
    }
}
