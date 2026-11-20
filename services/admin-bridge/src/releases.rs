//! The release workflow on GitHub (design §10): feature switch-on requests start
//! `cfmeet-coordinated-release.yml` on `main` through GitHub's `workflow_dispatch` REST API,
//! and HQ sees the workflow's runs as a cleaned-up release history.
//!
//! * The token is read from a file on every call (rotation needs no restart) and is never
//!   logged, echoed or stored; the request header carrying it is marked sensitive.
//! * No redirects, no proxy, bounded timeouts. Only the workflow's own endpoints are
//!   called: dispatch, and the runs list of that workflow.
//! * From a run only ids, the event, the commit, status, conclusion and times are kept:
//!   never logs, artifacts, inputs or anything the workflow printed.
//! * The bridge never touches Docker, the servers or secrets: the workflow's own gates
//!   (main only, the exact SHA, green CI, owner confirmations) still apply.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use zeroize::Zeroizing;

/// The release workflow (file name under `.github/workflows/`).
pub const WORKFLOW_FILE: &str = "cfmeet-coordinated-release.yml";
/// The only ref the workflow runs from.
pub const WORKFLOW_REF: &str = "main";
pub const DEFAULT_API_URL: &str = "https://api.github.com";

/// The release workflow's `operation` options, in the workflow's order. Kept in sync with
/// `.github/workflows/cfmeet-coordinated-release.yml` by a test that reads that file (the
/// container build context does not carry `.github/`, so it is not read at build time).
pub const RELEASE_OPERATIONS: &[&str] = &[
    "inspect",
    "backup",
    "provision",
    "release",
    "verify",
    "rollback",
    "certify",
    "activate-provisioning",
    "activate-org-events",
    "activate-organization-bindings",
    "org-cutover-export",
    "org-cutover-import",
    "org-cutover-shadow",
    "org-cutover-authoritative",
    "org-cutover-first-activation",
    "activate-enterprise-calling",
    "activate-developer-platform",
    "activate-gateway-routing",
    "activate-first-party-features",
    "activate-personal-encryption",
    "activate-web-push",
    "activate-web-origins",
    "activate-gateway-docs",
    "activate-edge-rate-limits",
    "activate-token-ceiling",
    "activate-service-key-publish",
    "activate-service-key-switch",
    "activate-service-key-retire",
    "activate-developer-console",
    "activate-developer-webhooks",
    "activate-gateway-families",
    "activate-usage-export",
    "activate-resend-email",
    "activate-production-access-review",
    "activate-meeting-notice-preferences",
    "activate-public-internal-denial",
    "activate-client-address-limits",
    "activate-account-privacy",
    "activate-database-pools",
    "activate-notifications-owners",
    "activate-notifications-routing",
    "activate-notifications-relay",
    "activate-messaging-publications",
    "activate-organizations-publications",
    "activate-communications-publications",
    "activate-developer-publications",
    "activate-meeting-publications",
    "activate-calling-publications",
    "activate-notifications-push",
    "activate-messaging-push-mirror",
    "activate-calling-push",
    "activate-critical-lane",
    "activate-preference-copy",
    "activate-preference-authority",
    "activate-notifications-email",
    "activate-notification-fence",
    "activate-meeting-in-app-notices",
    "activate-platform-users",
    "activate-platform-user-tokens",
    "activate-partner-chat",
    "activate-partner-calls",
    "activate-gateway-calling-edge",
    "activate-partner-push",
    "activate-platform-meetings",
    "activate-oauth-apps",
    "activate-oauth",
    "activate-delegated-apps",
    "activate-app-installs",
    "activate-app-events",
    "activate-app-directory",
    "activate-in-meeting-apps",
    "developer-acceptance",
    "organization-acceptance",
    "diagnose",
    "email-acceptance",
    "developer-access-review",
    "notifications-acceptance",
    "deploy-monitoring",
    "platform-acceptance",
];

/// Operations the workflow runs on any `release_sha` (its gate lets these differ from
/// the main head it was dispatched on).
pub const ANY_SHA_OPERATIONS: &[&str] = &["inspect", "diagnose", "rollback"];

/// Run statuses as published (GitHub's, upper-cased; anything else is `UNKNOWN`).
pub const RUN_STATUSES: &[&str] = &[
    "REQUESTED",
    "QUEUED",
    "PENDING",
    "WAITING",
    "IN_PROGRESS",
    "COMPLETED",
    "UNKNOWN",
];
/// Run conclusions as published (GitHub's, upper-cased; anything else is `UNKNOWN`).
pub const RUN_CONCLUSIONS: &[&str] = &[
    "SUCCESS",
    "FAILURE",
    "NEUTRAL",
    "CANCELLED",
    "SKIPPED",
    "TIMED_OUT",
    "ACTION_REQUIRED",
    "STALE",
    "STARTUP_FAILURE",
    "UNKNOWN",
];

/// `owner/name` as GitHub allows it.
pub fn repository_ok(v: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p.as_bytes()[0].is_ascii_alphanumeric()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    matches!(v.split_once('/'), Some((o, n)) if part(o) && part(n) && !n.contains('/'))
}

/// A full lowercase commit SHA.
pub fn sha_ok(v: &str) -> bool {
    v.len() == 40
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The workflow's `confirm` input: owner gate words (lowercase, comma separated, `k=v`).
pub fn confirm_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && (v.as_bytes()[0].is_ascii_lowercase() || v.as_bytes()[0].is_ascii_digit())
        && v.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'=' | b',' | b'.' | b'_' | b':' | b'-')
        })
}

/// Configuration (`ADMIN_BRIDGE_GITHUB_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSettings {
    pub api_url: String,
    pub repository: String,
    pub token_file: PathBuf,
}

/// What became of a dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// GitHub accepted it (204, or 200 with run details).
    Dispatched,
    /// GitHub refused it (3xx/4xx): no run was started.
    Refused { status: u16 },
    /// It never reached GitHub (no usable token, connection not established).
    NotSent,
    /// Sent, and no clear answer: settled from the runs list, never resent.
    Unknown,
}

/// One workflow run, reduced to what HQ may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    pub id: i64,
    pub event: String,
    pub head_sha: String,
    pub head_branch: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One page of the workflow's runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPage {
    pub total_count: u64,
    pub runs: Vec<WorkflowRun>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GitHubError {
    #[error("GitHub did not answer the runs list")]
    Unavailable,
    #[error("GitHub answered outside the expected shape")]
    Contract,
}

/// The release workflow client.
pub struct ReleaseWorkflow {
    client: reqwest::Client,
    api_url: String,
    repository: String,
    token_file: PathBuf,
}

impl std::fmt::Debug for ReleaseWorkflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseWorkflow")
            .field("api_url", &self.api_url)
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

fn upper(v: &str, allowed: &[&str]) -> String {
    let u = v.to_ascii_uppercase();
    if allowed.contains(&u.as_str()) {
        u
    } else {
        "UNKNOWN".into()
    }
}

fn parse_run(v: &Value) -> Option<WorkflowRun> {
    let time = |k: &str| {
        DateTime::parse_from_rfc3339(v.get(k)?.as_str()?)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    };
    let id = v.get("id")?.as_i64().filter(|i| *i >= 1)?;
    let event = v
        .get("event")?
        .as_str()
        .filter(|e| {
            !e.is_empty()
                && e.len() <= 64
                && e.as_bytes()[0].is_ascii_lowercase()
                && e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })?
        .to_string();
    let head_sha = v
        .get("head_sha")?
        .as_str()
        .filter(|s| sha_ok(s))?
        .to_string();
    let head_branch = v
        .get("head_branch")
        .and_then(Value::as_str)
        .map(str::to_string);
    let status = upper(v.get("status")?.as_str()?, RUN_STATUSES);
    let conclusion = match v.get("conclusion") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) => Some(upper(c, RUN_CONCLUSIONS)),
        _ => return None,
    };
    Some(WorkflowRun {
        id,
        event,
        head_sha,
        head_branch,
        status,
        conclusion,
        created_at: time("created_at")?,
        updated_at: time("updated_at")?,
    })
}

impl ReleaseWorkflow {
    pub fn new(settings: &GitHubSettings) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent("cfmeet-admin-bridge")
            .build()
            .expect("http client");
        ReleaseWorkflow {
            client,
            api_url: settings.api_url.trim_end_matches('/').to_string(),
            repository: settings.repository.clone(),
            token_file: settings.token_file.clone(),
        }
    }

    /// The token, read now. `None` when the file is missing, empty or not a plain token.
    fn token(&self) -> Option<Zeroizing<String>> {
        let raw = Zeroizing::new(std::fs::read_to_string(&self.token_file).ok()?);
        let t = raw.trim();
        (!t.is_empty() && t.len() <= 512 && t.bytes().all(|b| b.is_ascii_graphic()))
            .then(|| Zeroizing::new(t.to_string()))
    }

    /// True when a token can be read now (checked before any change is recorded).
    pub fn usable(&self) -> bool {
        self.token().is_some()
    }

    fn workflow_url(&self, tail: &str) -> String {
        format!(
            "{}/repos/{}/actions/workflows/{WORKFLOW_FILE}/{tail}",
            self.api_url, self.repository
        )
    }

    fn request(
        &self,
        method: reqwest::Method,
        url: &str,
        token: &str,
    ) -> Option<reqwest::RequestBuilder> {
        let mut auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).ok()?;
        auth.set_sensitive(true);
        Some(
            self.client
                .request(method, url)
                .header(reqwest::header::AUTHORIZATION, auth)
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
        )
    }

    /// Starts the workflow on `main`, once. Never retried.
    pub async fn dispatch(
        &self,
        operation: &str,
        release_sha: &str,
        confirm: Option<&str>,
    ) -> DispatchOutcome {
        let Some(token) = self.token() else {
            return DispatchOutcome::NotSent;
        };
        let mut inputs = json!({"operation": operation, "release_sha": release_sha});
        if let Some(c) = confirm {
            inputs["confirm"] = json!(c);
        }
        let body = serde_json::to_vec(&json!({"ref": WORKFLOW_REF, "inputs": inputs}))
            .expect("serializable");
        let Some(req) = self.request(
            reqwest::Method::POST,
            &self.workflow_url("dispatches"),
            &token,
        ) else {
            return DispatchOutcome::NotSent;
        };
        let sent = req
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        match sent {
            Ok(r) => {
                let status = r.status().as_u16();
                match status {
                    200..=299 => DispatchOutcome::Dispatched,
                    300..=499 => DispatchOutcome::Refused { status },
                    _ => DispatchOutcome::Unknown,
                }
            }
            // The connection was never established: GitHub never saw the request.
            Err(e) if e.is_connect() => DispatchOutcome::NotSent,
            Err(_) => DispatchOutcome::Unknown,
        }
    }

    /// The workflow's runs, newest first. `created` is GitHub's `created` filter
    /// (`>=<time>` or `<start>..<end>`).
    pub async fn runs(
        &self,
        created: &str,
        per_page: u32,
        page: u32,
    ) -> Result<RunPage, GitHubError> {
        let token = self.token().ok_or(GitHubError::Unavailable)?;
        let req = self
            .request(reqwest::Method::GET, &self.workflow_url("runs"), &token)
            .ok_or(GitHubError::Unavailable)?
            .query(&[
                ("branch", WORKFLOW_REF.to_string()),
                ("created", created.to_string()),
                ("per_page", per_page.clamp(1, 100).to_string()),
                ("page", page.max(1).to_string()),
            ]);
        let resp = req.send().await.map_err(|_| GitHubError::Unavailable)?;
        if !resp.status().is_success() {
            return Err(GitHubError::Unavailable);
        }
        let bytes = resp.bytes().await.map_err(|_| GitHubError::Unavailable)?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| GitHubError::Contract)?;
        let total_count = v
            .get("total_count")
            .and_then(Value::as_u64)
            .ok_or(GitHubError::Contract)?;
        let runs = v
            .get("workflow_runs")
            .and_then(Value::as_array)
            .ok_or(GitHubError::Contract)?
            .iter()
            .map(parse_run)
            .collect::<Option<Vec<_>>>()
            .ok_or(GitHubError::Contract)?;
        Ok(RunPage { total_count, runs })
    }
}

/// GitHub's time filter format.
pub fn github_time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_validated() {
        assert!(repository_ok("CosmicforgeHealthnet/cfmeet"));
        assert!(!repository_ok("cfmeet"));
        assert!(!repository_ok("a/b/c"));
        assert!(!repository_ok("../x/y"));
        assert!(sha_ok(&"a".repeat(40)));
        assert!(!sha_ok(&"A".repeat(40)) && !sha_ok("abc"));
        assert!(confirm_ok("service=meeting-service") && confirm_ok("otp"));
        assert!(!confirm_ok("") && !confirm_ok("Enable") && !confirm_ok("a b"));
    }

    #[test]
    fn runs_keep_only_what_hq_may_see() {
        let run = parse_run(&json!({
            "id": 42, "event": "workflow_dispatch", "head_sha": "a".repeat(40), "head_branch": "main",
            "status": "in_progress", "conclusion": null, "created_at": "2026-11-20T10:00:00Z",
            "updated_at": "2026-11-20T10:01:00Z", "display_title": "x", "actor": {"login": "someone"}
        }))
        .unwrap();
        assert_eq!((run.id, run.status.as_str(), run.conclusion), (42, "IN_PROGRESS", None));
        let odd = parse_run(&json!({
            "id": 43, "event": "workflow_dispatch", "head_sha": "b".repeat(40),
            "status": "brand_new", "conclusion": "weird", "created_at": "2026-11-20T10:00:00Z",
            "updated_at": "2026-11-20T10:01:00Z"
        }))
        .unwrap();
        assert_eq!((odd.status.as_str(), odd.conclusion.as_deref()), ("UNKNOWN", Some("UNKNOWN")));
        assert!(parse_run(&json!({"id": 1, "event": "x", "head_sha": "zz"})).is_none());
    }
}
