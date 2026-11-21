//! CF Meet user plan limits (ADR 0068 A6, design §8 "Enforcement", brief §4).
//!
//! The Developer Platform owns the Free / Pro / Business plans and their assignment to a
//! user or business organization. Messaging enforces the plan limits where it already
//! enforces a quota:
//!
//! | Plan limit | Where | Refusal |
//! |---|---|---|
//! | `attachmentBytes` | an upload intent in CF Meet's own environment (the uploader's CF Meet account, holder kind `USER`) | the existing `QUOTA_EXCEEDED`, reason `PLAN_ATTACHMENT_LIMIT` |
//!
//! The plan can only tighten what the application policy already allows (the smaller limit
//! wins). The holder's limits come from a signed internal read of the Developer Platform
//! (`GET /internal/v1/plan-limits/{holderKind}/{holderId}`, permission
//! `developer-entitlement.read` → `{planCode, limits: {name: int|bool|null}, cacheSeconds,
//! ...}`; a `null` or absent size limit = unlimited, booleans are feature flags Messaging
//! does not use), cached for `MESSAGING_PLAN_CACHE_SECONDS` (at most the answer's
//! `cacheSeconds`).
//!
//! **Off by default** (`MESSAGING_PLAN_ENFORCEMENT=on` and `CF_DEVELOPER_PLATFORM_BASE_URL`
//! turn it on). **Fail open**: when the read is unconfigured, refused or unreachable, or
//! the uploader is not a CF Meet account, today's behaviour is kept (the policy limits
//! only); a failed read is cached briefly so a Developer Platform outage is not hammered.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{identity::TrustedContext, state::AppState};

/// The plan limit capping one attachment's size.
pub const ATTACHMENT_BYTES: &str = "attachmentBytes";
/// Holder kind of a CF Meet user.
pub const USER: &str = "USER";
/// Most holders cached at once (the cache is cleared beyond).
const MAX_CACHED: usize = 10_000;
/// How long a failed read is remembered (fail open meanwhile).
const FAILURE_TTL: Duration = Duration::from_secs(15);

/// `MESSAGING_PLAN_ENFORCEMENT` / `MESSAGING_PLAN_CACHE_SECONDS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanSettings {
    pub enabled: bool,
    pub cache_ttl: Duration,
}

impl Default for PlanSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_ttl: Duration::from_secs(60),
        }
    }
}

impl PlanSettings {
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut s = Self::default();
        match get("MESSAGING_PLAN_ENFORCEMENT").as_deref().map(str::trim) {
            None | Some("") | Some("off") => {}
            Some("on") => s.enabled = true,
            Some(_) => return Err("MESSAGING_PLAN_ENFORCEMENT must be off or on".into()),
        }
        if let Some(v) = get("MESSAGING_PLAN_CACHE_SECONDS").filter(|v| !v.trim().is_empty()) {
            let secs = v
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|n| (5..=3600).contains(n))
                .ok_or("MESSAGING_PLAN_CACHE_SECONDS must be an integer between 5 and 3600")?;
            s.cache_ttl = Duration::from_secs(secs);
        }
        Ok(s)
    }
}

/// A holder's effective plan limits (`None` = unlimited).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanLimits {
    pub plan_code: String,
    #[serde(default, deserialize_with = "integer_limits")]
    pub limits: HashMap<String, Option<i64>>,
    /// The Developer Platform's bound on the caller's cache.
    #[serde(default)]
    pub cache_seconds: Option<u64>,
}

/// Integer limits only; a boolean or other value (a feature flag) is not a size limit.
fn integer_limits<'de, D>(d: D) -> Result<HashMap<String, Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: HashMap<String, Value> = HashMap::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|(k, v)| match v {
            Value::Null => Some((k, None)),
            Value::Number(n) => n.as_i64().filter(|n| *n >= 0).map(|n| (k, Some(n))),
            _ => None,
        })
        .collect())
}

/// The plan read failed (unconfigured, refused, unreachable or malformed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanUnavailable;

#[async_trait]
pub trait PlanLimitsSource: Send + Sync {
    async fn limits(
        &self,
        holder_kind: &str,
        holder_id: Uuid,
    ) -> Result<PlanLimits, PlanUnavailable>;
}

/// The Developer Platform's plan read (service token, `cfmeet.developer-platform`).
pub struct DeveloperPlatformPlans {
    client: Arc<InternalServiceClient>,
}

impl DeveloperPlatformPlans {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

/// The read's path for one holder.
pub fn limits_path(holder_kind: &str, holder_id: Uuid) -> String {
    format!("/internal/v1/plan-limits/{holder_kind}/{holder_id}")
}

#[async_trait]
impl PlanLimitsSource for DeveloperPlatformPlans {
    async fn limits(
        &self,
        holder_kind: &str,
        holder_id: Uuid,
    ) -> Result<PlanLimits, PlanUnavailable> {
        let request = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let path = limits_path(holder_kind, holder_id);
        match self
            .client
            .exchange_json::<Value>(&request, Method::GET, &path, None)
            .await
        {
            Ok(r) if r.status() == 200 => r.json::<PlanLimits>().map_err(|_| PlanUnavailable),
            _ => Err(PlanUnavailable),
        }
    }
}

type Cached = (Instant, Option<PlanLimits>);

/// The cached plan limits Messaging enforces (present only when enforcement is on).
pub struct PlanEnforcement {
    source: Arc<dyn PlanLimitsSource>,
    ttl: Duration,
    cache: Mutex<HashMap<(String, Uuid), Cached>>,
}

impl PlanEnforcement {
    pub fn new(source: Arc<dyn PlanLimitsSource>, ttl: Duration) -> Self {
        Self {
            source,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The holder's limit `name`: `None` when unlimited, unknown or unavailable (fail open).
    pub async fn limit(&self, holder_kind: &str, holder_id: Uuid, name: &str) -> Option<i64> {
        let key = (holder_kind.to_string(), holder_id);
        let cached = self
            .cache
            .lock()
            .expect("plan cache")
            .get(&key)
            .filter(|(until, _)| *until > Instant::now())
            .map(|(_, l)| l.clone());
        let limits = match cached {
            Some(l) => l,
            None => {
                let fetched = self.source.limits(holder_kind, holder_id).await;
                let (ttl, value) = match fetched {
                    Ok(l) => (
                        l.cache_seconds
                            .map_or(self.ttl, |s| self.ttl.min(Duration::from_secs(s))),
                        Some(l),
                    ),
                    Err(PlanUnavailable) => {
                        crate::metrics::inc(
                            "messaging_plan_reads_total",
                            &[("outcome", "unavailable")],
                        );
                        tracing::warn!(
                            "plan limits unavailable: the current limits apply (fail open)"
                        );
                        (self.ttl.min(FAILURE_TTL), None)
                    }
                };
                let mut cache = self.cache.lock().expect("plan cache");
                if cache.len() >= MAX_CACHED {
                    cache.clear();
                }
                cache.insert(key, (Instant::now() + ttl, value.clone()));
                value
            }
        };
        limits.and_then(|l| l.limits.get(name).copied().flatten())
    }
}

/// The plan cap on one attachment's size for `uploader` in `ctx`'s environment, or `None`
/// (enforcement off, not CF Meet's own environment, not a CF Meet account, no limit, or
/// the plan read unavailable: today's limits apply).
pub async fn attachment_cap(state: &AppState, ctx: &TrustedContext, uploader: Uuid) -> Option<i64> {
    let plans = state.plans.as_ref()?;
    let first_party =
        crate::conversations::policy::load_baseline(&state.db, ctx.scope.environment_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|b| {
                b == crate::conversations::policy::ApplicationBaseline::CfMeetFirstParty
            });
    if !first_party {
        return None;
    }
    let refs = state
        .principals
        .native_user_references(&ctx.scope, &[uploader])
        .await
        .ok()?;
    let account = refs
        .get(&uploader)
        .and_then(|r| crate::notifications::account_ref(r))?;
    let id =
        Uuid::parse_str(account.strip_prefix(crate::notifications::ACCOUNT_REF_PREFIX)?).ok()?;
    plans.limit(USER, id, ATTACHMENT_BYTES).await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Fake {
        calls: AtomicUsize,
        answer: Result<PlanLimits, PlanUnavailable>,
    }

    #[async_trait]
    impl PlanLimitsSource for Fake {
        async fn limits(&self, _: &str, _: Uuid) -> Result<PlanLimits, PlanUnavailable> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn pro() -> PlanLimits {
        serde_json::from_value(serde_json::json!({
            "planCode": "PRO",
            "limits": { "attachmentBytes": 50_000_000, "storageBytes": null, "recording": true },
            "holderKind": "USER", "assigned": true, "cacheSeconds": 300
        }))
        .unwrap()
    }

    #[test]
    fn settings_are_off_by_default_and_validated() {
        let none = |_: &str| None;
        assert_eq!(
            PlanSettings::from_env(none).unwrap(),
            PlanSettings::default()
        );
        assert!(!PlanSettings::default().enabled);
        let on = |k: &str| match k {
            "MESSAGING_PLAN_ENFORCEMENT" => Some("on".to_string()),
            "MESSAGING_PLAN_CACHE_SECONDS" => Some("30".to_string()),
            _ => None,
        };
        let s = PlanSettings::from_env(on).unwrap();
        assert!(s.enabled);
        assert_eq!(s.cache_ttl, Duration::from_secs(30));
        assert!(PlanSettings::from_env(
            |k: &str| (k == "MESSAGING_PLAN_ENFORCEMENT").then(|| "yes".to_string())
        )
        .is_err());
        assert!(PlanSettings::from_env(
            |k: &str| (k == "MESSAGING_PLAN_CACHE_SECONDS").then(|| "1".to_string())
        )
        .is_err());
    }

    #[test]
    fn only_integer_limits_are_read() {
        let p = pro();
        assert_eq!(p.plan_code, "PRO");
        assert_eq!(p.limits.get(ATTACHMENT_BYTES), Some(&Some(50_000_000)));
        assert_eq!(p.limits.get("storageBytes"), Some(&None));
        assert!(!p.limits.contains_key("recording"));
        assert_eq!(p.cache_seconds, Some(300));
        assert_eq!(
            limits_path(USER, Uuid::nil()),
            format!("/internal/v1/plan-limits/USER/{}", Uuid::nil())
        );
    }

    #[tokio::test]
    async fn limits_are_cached_and_fail_open() {
        let fake = Arc::new(Fake {
            calls: AtomicUsize::new(0),
            answer: Ok(pro()),
        });
        let p = PlanEnforcement::new(fake.clone(), Duration::from_secs(60));
        let holder = Uuid::new_v4();
        assert_eq!(
            p.limit(USER, holder, ATTACHMENT_BYTES).await,
            Some(50_000_000)
        );
        assert_eq!(p.limit(USER, holder, "storageBytes").await, None);
        assert_eq!(p.limit(USER, holder, "unknown").await, None);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "cached");

        let down = Arc::new(Fake {
            calls: AtomicUsize::new(0),
            answer: Err(PlanUnavailable),
        });
        let p = PlanEnforcement::new(down.clone(), Duration::from_secs(60));
        assert_eq!(p.limit(USER, holder, ATTACHMENT_BYTES).await, None);
        assert_eq!(p.limit(USER, holder, ATTACHMENT_BYTES).await, None);
        assert_eq!(
            down.calls.load(Ordering::SeqCst),
            1,
            "failures are cached briefly"
        );
    }
}
