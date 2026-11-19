//! S2 — account deletion and the data download across every service that holds the
//! person's data.
//!
//! The Meeting Service is the account authority; Messaging, the Communications Hub and
//! Calling keep their own data about the same person (chats, contacts / CF Meet ID / saved
//! messages, call history). Signing them out is not deleting it, so:
//!
//! * **Deletion** — [`queue_erasure`] runs inside the transaction that anonymizes the
//!   account and queues one durable job per owner (`account_privacy_jobs`, unique per
//!   account and target, so a repeat queues nothing). [`deliver_due`] (the privacy worker,
//!   [`spawn`], and right after a deletion) delivers them with the Meeting Service's own
//!   signed service token and the CF Meet FIRST_PARTY scope:
//!   * `MESSAGING` — `POST /internal/v1/delegated/v1/messaging/deletion-requests`
//!     `{kind: PRINCIPAL_ERASURE, principalId}` (grant `messaging-application.delegate`),
//!     then its status until `COMPLETED` / `PARTIALLY_HELD` (a legal hold wins; the held
//!     items are counted in the result);
//!   * `COMMUNICATIONS` — `POST /internal/v1/communications/account-erasure`
//!     `{accountReference}` (grant `communications-account.apply`);
//!   * `CALLING` — `POST /internal/v1/account-privacy/erasure` `{principalId}` (grant
//!     `calling-account.privacy`), repeated until no Call of the person is still live.
//!
//!   An account that never had a communication principal has nothing at Messaging or
//!   Calling: those jobs end at once (`principal: false`). Every failure (owner down, grant
//!   not yet applied, configuration missing) leaves the job pending with an exponential
//!   backoff capped at one hour; nothing is dropped. Each owner's operation is idempotent.
//! * **Download** — [`export_elsewhere`] reads the same three owners back for the account's
//!   single JSON document: Messaging conversations the person is in (as the person), the
//!   Hub's profile, CF Meet ID, contacts, lists and saved messages, and Calling's history.
//!   An owner that cannot answer is reported as `unavailable` in the document instead of
//!   failing the whole download.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind, KeyId,
    OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    SystemClock, TokenTtl,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiResult;

/// The owner services a deleted account's data is erased at.
pub const TARGETS: [&str; 3] = ["MESSAGING", "COMMUNICATIONS", "CALLING"];
/// Retry backoff ceiling.
const MAX_BACKOFF_SECS: i64 = 3600;
/// Jobs per worker pass.
const BATCH: i64 = 20;
/// Conversations listed per page / pages read for the download.
const CONVERSATION_PAGE: &str = "100";
const CONVERSATION_PAGES: usize = 10;

/// The signed client of the three owners (built once from the environment).
pub struct PrivacyClients {
    client: Arc<InternalServiceClient>,
    scope: DelegatedPlatformContext,
    messaging: bool,
    communications: bool,
    calling: bool,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl PrivacyClients {
    /// `None` when the Meeting Service has no signing identity or no owner URL at all (a
    /// local setup): jobs then stay pending until it is configured.
    pub fn from_env() -> Result<Option<Self>, String> {
        let urls = [
            (ServiceId::messaging(), env("CF_MESSAGING_BASE_URL")),
            (
                ServiceId::communications(),
                env("CF_COMMUNICATIONS_BASE_URL"),
            ),
            (ServiceId::calling(), env("CF_CALLING_BASE_URL")),
        ];
        if urls.iter().all(|(_, u)| u.is_none()) {
            return Ok(None);
        }
        let (Some(runtime), Some(kid), Some(key_file)) = (
            env("CF_RUNTIME_ENVIRONMENT"),
            env("CF_SERVICE_AUTH_KEY_ID"),
            env("CF_SERVICE_AUTH_PRIVATE_KEY_FILE"),
        ) else {
            return Ok(None);
        };
        let runtime = RuntimeEnvironment::parse(&runtime).map_err(|_| "invalid runtime")?;
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&kid).map_err(|_| "invalid service key id")?,
            &PathBuf::from(key_file),
        )
        .map_err(|_| "service signing key is unavailable")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid TTL")?,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(runtime);
        let mut present = [false; 3];
        for (i, (service, url)) in urls.iter().enumerate() {
            if let Some(url) = url {
                targets
                    .insert(service.clone(), url)
                    .map_err(|_| format!("invalid {service} URL"))?;
                present[i] = true;
            }
        }
        let client = Arc::new(
            InternalServiceClient::new(signer, targets, Duration::from_secs(10))
                .map_err(|_| "invalid privacy client")?,
        );
        let id = |name: &str| {
            env(name)
                .and_then(|v| Uuid::parse_str(&v).ok())
                .ok_or_else(|| format!("{name} is required for account erasure"))
        };
        let kind = match env("CFMEET_COMMUNICATION_ENVIRONMENT_KIND").as_deref() {
            Some("PRODUCTION") => ApplicationEnvironmentKind::Production,
            Some("TEST") => ApplicationEnvironmentKind::Test,
            Some("SANDBOX") => ApplicationEnvironmentKind::Sandbox,
            _ => return Err("CFMEET_COMMUNICATION_ENVIRONMENT_KIND is required".into()),
        };
        Ok(Some(Self {
            client,
            scope: DelegatedPlatformContext::new(
                id("CFMEET_COMMUNICATION_TENANT_ID")?,
                id("CFMEET_COMMUNICATION_APPLICATION_ID")?,
                id("CFMEET_COMMUNICATION_ENVIRONMENT_ID")?,
                kind,
                id("CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID")?,
            ),
            messaging: present[0],
            communications: present[1],
            calling: present[2],
        }))
    }

    fn request(&self, to: ServiceId, acting: Option<Uuid>) -> OutboundServiceRequest {
        let scope = match acting {
            Some(p) => self
                .scope
                .clone()
                .with_initiating_principal(p, DelegatedPrincipalKind::NativeCfUser),
            None => self.scope.clone(),
        };
        OutboundServiceRequest::new_operation(to).with_delegated_context(scope)
    }

    fn configured(&self, target: &str) -> bool {
        match target {
            "MESSAGING" => self.messaging,
            "COMMUNICATIONS" => self.communications,
            "CALLING" => self.calling,
            _ => false,
        }
    }
}

/// The process-wide clients (`None`: not configured; a configuration error is logged once
/// and treated the same, so a deletion is never refused for it).
pub fn clients() -> Option<&'static PrivacyClients> {
    static CLIENTS: OnceLock<Option<PrivacyClients>> = OnceLock::new();
    CLIENTS
        .get_or_init(|| match PrivacyClients::from_env() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("account privacy clients are not configured: {e}");
                None
            }
        })
        .as_ref()
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Queues the erasure jobs of `user_id` (inside the deletion transaction). Idempotent.
pub async fn queue_erasure<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<()> {
    for target in TARGETS {
        db.execute(stmt(
            "INSERT INTO account_privacy_jobs (id, user_id, target) VALUES ($1, $2, $3) \
             ON CONFLICT (user_id, target) DO NOTHING",
            vec![Uuid::new_v4().into(), user_id.into(), target.into()],
        ))
        .await?;
    }
    Ok(())
}

/// The person's communication principal in the CF Meet scope, if they ever had one.
async fn principal_of<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    environment_id: Uuid,
) -> ApiResult<Option<Uuid>> {
    let row = db
        .query_one(stmt(
            "SELECT principal_id FROM communication_principal_links \
              WHERE subject_kind = 'USER' AND subject_id = $1 AND environment_id = $2",
            vec![user_id.into(), environment_id.into()],
        ))
        .await?;
    Ok(row.and_then(|r| r.try_get::<Uuid>("", "principal_id").ok()))
}

/// What one delivery attempt achieved.
#[derive(Debug, PartialEq)]
enum Step {
    /// The owner confirmed; the job is done with this result.
    Done(Value),
    /// The owner accepted a request still in progress (Messaging's request id).
    Submitted(Uuid),
    /// Not finished yet (a live call, a running request): try again later.
    Again(String),
}

/// Messaging's terminal deletion request states.
fn messaging_finished(status: &str) -> bool {
    matches!(status, "COMPLETED" | "PARTIALLY_HELD")
}

async fn deliver_one(
    c: &PrivacyClients,
    db: &DatabaseConnection,
    user_id: Uuid,
    target: &str,
    remote: Option<Uuid>,
) -> Result<Step, String> {
    if !c.configured(target) {
        return Err(format!("{target} is not configured"));
    }
    let environment = c.scope.application_environment_id();
    let short = |e: cf_service_auth::client::InternalCallError| e.to_string();
    match target {
        "COMMUNICATIONS" => {
            let r = c
                .client
                .post_json(
                    &c.request(ServiceId::communications(), None),
                    "/internal/v1/communications/account-erasure",
                    &json!({"accountReference": format!("cfmeet-user:{user_id}")}),
                )
                .await
                .map_err(short)?;
            Ok(Step::Done(r.json::<Value>().map_err(short)?))
        }
        "CALLING" => {
            let Some(principal) = principal_of(db, user_id, environment)
                .await
                .map_err(|e| e.to_string())?
            else {
                return Ok(Step::Done(json!({"principal": false})));
            };
            let r = c
                .client
                .post_json(
                    &c.request(ServiceId::calling(), None),
                    "/internal/v1/account-privacy/erasure",
                    &json!({"principalId": principal}),
                )
                .await
                .map_err(short)?;
            let body: Value = r.json().map_err(short)?;
            if body["complete"] == json!(true) {
                Ok(Step::Done(body))
            } else {
                Ok(Step::Again("a call of the account is still live".into()))
            }
        }
        "MESSAGING" => {
            let Some(principal) = principal_of(db, user_id, environment)
                .await
                .map_err(|e| e.to_string())?
            else {
                return Ok(Step::Done(json!({"principal": false})));
            };
            let base = "/internal/v1/delegated/v1/messaging/deletion-requests";
            let Some(request_id) = remote else {
                let r = c
                    .client
                    .post_json(
                        &c.request(ServiceId::messaging(), None),
                        base,
                        &json!({"kind": "PRINCIPAL_ERASURE", "principalId": principal}),
                    )
                    .await
                    .map_err(short)?;
                let body: Value = r.json().map_err(short)?;
                return body["requestId"]
                    .as_str()
                    .and_then(|v| Uuid::parse_str(v).ok())
                    .map(Step::Submitted)
                    .ok_or_else(|| "Messaging answered without a request id".to_string());
            };
            let r = c
                .client
                .get(
                    &c.request(ServiceId::messaging(), None),
                    &format!("{base}/{request_id}"),
                )
                .await
                .map_err(short)?;
            let body: Value = r.json().map_err(short)?;
            if messaging_finished(body["status"].as_str().unwrap_or_default()) {
                Ok(Step::Done(body))
            } else {
                Ok(Step::Again(
                    "the Messaging deletion request is still running".into(),
                ))
            }
        }
        other => Err(format!("unknown target {other}")),
    }
}

/// Exponential backoff for attempt `n` (seconds, capped at an hour).
fn backoff_secs(attempts: i32) -> i64 {
    2i64.saturating_pow(attempts.clamp(0, 12) as u32)
        .min(MAX_BACKOFF_SECS)
}

/// One pass of the privacy worker: leases due jobs and delivers them. Returns how many
/// finished. Safe on several instances (`FOR UPDATE SKIP LOCKED` + lease).
pub async fn deliver_due(db: &DatabaseConnection) -> ApiResult<u64> {
    let Some(c) = clients() else {
        return Ok(0);
    };
    let lease = Uuid::new_v4();
    let rows = db
        .query_all(stmt(
            "WITH due AS (SELECT id FROM account_privacy_jobs \
                           WHERE state <> 'DONE' AND next_attempt_at <= now() \
                             AND (lease_until IS NULL OR lease_until < now()) \
                           ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
             UPDATE account_privacy_jobs j SET lease_id = $1, lease_until = now() + interval '120 seconds', \
                    attempts = attempts + 1 \
               FROM due WHERE j.id = due.id \
             RETURNING j.id, j.user_id, j.target, j.remote_request_id, j.attempts",
            vec![lease.into(), BATCH.into()],
        ))
        .await?;
    let mut finished = 0;
    for row in rows {
        let id: Uuid = row.try_get("", "id")?;
        let user_id: Uuid = row.try_get("", "user_id")?;
        let target: String = row.try_get("", "target")?;
        let remote: Option<Uuid> = row.try_get("", "remote_request_id")?;
        let attempts: i32 = row.try_get("", "attempts")?;
        let step = deliver_one(c, db, user_id, &target, remote).await;
        let (sql, values): (&str, Vec<sea_orm::Value>) = match step {
            Ok(Step::Done(result)) => {
                finished += 1;
                (
                    "UPDATE account_privacy_jobs SET state = 'DONE', result = $3, completed_at = now(), \
                            last_error = NULL, lease_id = NULL, lease_until = NULL \
                      WHERE id = $1 AND lease_id = $2",
                    vec![id.into(), lease.into(), result.into()],
                )
            }
            Ok(Step::Submitted(request)) => (
                "UPDATE account_privacy_jobs SET state = 'SUBMITTED', remote_request_id = $3, \
                        next_attempt_at = now() + interval '30 seconds', last_error = NULL, \
                        lease_id = NULL, lease_until = NULL WHERE id = $1 AND lease_id = $2",
                vec![id.into(), lease.into(), request.into()],
            ),
            Ok(Step::Again(why)) | Err(why) => {
                tracing::warn!(job = %id, target, attempts, "account erasure not finished yet: {why}");
                (
                    "UPDATE account_privacy_jobs SET next_attempt_at = now() + make_interval(secs => $3), \
                            last_error = $4, lease_id = NULL, lease_until = NULL \
                      WHERE id = $1 AND lease_id = $2",
                    vec![
                        id.into(),
                        lease.into(),
                        (backoff_secs(attempts) as f64).into(),
                        why.chars().take(200).collect::<String>().into(),
                    ],
                )
            }
        };
        db.execute(stmt(sql, values)).await?;
    }
    Ok(finished)
}

/// The privacy worker: one pass every `interval` (and one right after each deletion).
pub fn spawn(db: DatabaseConnection, interval: Duration) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = deliver_due(&db).await {
                tracing::error!("account privacy worker pass failed: {e}");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// The other owners' data about `user_id` for the account's download (see the module
/// documentation). Never fails: an owner that cannot answer is reported as unavailable.
pub async fn export_elsewhere(db: &DatabaseConnection, user_id: Uuid) -> Value {
    let unavailable = |why: &str| json!({"status": "unavailable", "reason": why});
    let Some(c) = clients() else {
        let v = unavailable("not configured");
        return json!({"messaging": v, "communications": v, "calling": v});
    };
    let principal = principal_of(db, user_id, c.scope.application_environment_id())
        .await
        .ok()
        .flatten();
    let communications = if c.communications {
        match c
            .client
            .post_json(
                &c.request(ServiceId::communications(), None),
                "/internal/v1/communications/account-export",
                &json!({"accountReference": format!("cfmeet-user:{user_id}")}),
            )
            .await
            .and_then(|r| r.json::<Value>())
        {
            Ok(v) => v,
            Err(e) => unavailable(&e.to_string()),
        }
    } else {
        unavailable("not configured")
    };
    let calling = match (c.calling, principal) {
        (false, _) => unavailable("not configured"),
        (true, None) => json!({"calls": []}),
        (true, Some(p)) => match c
            .client
            .post_json(
                &c.request(ServiceId::calling(), None),
                "/internal/v1/account-privacy/export",
                &json!({"principalId": p}),
            )
            .await
            .and_then(|r| r.json::<Value>())
        {
            Ok(v) => v,
            Err(e) => unavailable(&e.to_string()),
        },
    };
    let messaging = match (c.messaging, principal) {
        (false, _) => unavailable("not configured"),
        (true, None) => json!({"conversations": []}),
        (true, Some(p)) => {
            let mut conversations = Vec::new();
            let mut cursor: Option<String> = None;
            let mut failure = None;
            for _ in 0..CONVERSATION_PAGES {
                let mut query = vec![("limit", CONVERSATION_PAGE)];
                if let Some(cur) = cursor.as_deref() {
                    query.push(("cursor", cur));
                }
                match c
                    .client
                    .get_query(
                        &c.request(ServiceId::messaging(), Some(p)),
                        "/internal/v1/delegated/v1/conversations",
                        &query,
                    )
                    .await
                    .and_then(|r| r.json::<Value>())
                {
                    Ok(page) => {
                        if let Some(items) = page["conversations"].as_array() {
                            conversations.extend(items.iter().cloned());
                        }
                        cursor = page["nextCursor"].as_str().map(str::to_owned);
                        if cursor.is_none() {
                            break;
                        }
                    }
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                }
            }
            match failure {
                Some(why) if conversations.is_empty() => unavailable(&why),
                _ => json!({
                    "conversations": conversations,
                    "truncated": cursor.is_some(),
                    "note": "Message contents of end-to-end encrypted chats exist only on your devices.",
                }),
            }
        }
    };
    json!({"messaging": messaging, "communications": communications, "calling": calling})
}

/// The erasure state of a deleted account (operators / tests).
pub async fn erasure_state<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<Value> {
    let rows = db
        .query_all(stmt(
            "SELECT target, state, attempts, last_error FROM account_privacy_jobs WHERE user_id = $1 ORDER BY target",
            vec![user_id.into()],
        ))
        .await?;
    let mut out = serde_json::Map::new();
    for r in rows {
        let target: String = r.try_get("", "target")?;
        out.insert(
            target,
            json!({
                "state": r.try_get::<String>("", "state")?,
                "attempts": r.try_get::<i32>("", "attempts")?,
                "lastError": r.try_get::<Option<String>>("", "last_error")?,
            }),
        );
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped_at_an_hour() {
        assert_eq!(backoff_secs(1), 2);
        assert_eq!(backoff_secs(5), 32);
        assert_eq!(backoff_secs(12), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(1000), MAX_BACKOFF_SECS);
        assert_eq!(backoff_secs(-3), 1);
    }

    #[test]
    fn only_terminal_messaging_requests_finish_the_job() {
        assert!(messaging_finished("COMPLETED"));
        assert!(messaging_finished("PARTIALLY_HELD"));
        assert!(!messaging_finished("PENDING"));
        assert!(!messaging_finished("RUNNING"));
        assert!(!messaging_finished(""));
    }
}
