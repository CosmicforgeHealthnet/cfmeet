//! ADR 0068 A6 — enforcement of the CF Meet user plans (Free / Pro / Business; design §8,
//! brief 2 §4). The Developer Platform owns plans, assignments and overrides; the Meeting
//! Service enforces the effective limits where the usage happens, on top of the limits it
//! already enforces:
//!
//! | Plan limit (Developer Platform key) | Where | Refusal (existing code) |
//! |---|---|---|
//! | `participants` | every seat taken in a conference ([`crate::services::admission`]) — all live seats, interactive and viewers | `409 MEETING_CAPACITY_REACHED` |
//! | `recording` (0 / false: not included) | starting a recording | `403 RECORDING_NOT_ALLOWED` |
//! | `meetingMinutes` | extending a live conference (where the 24-hour cap is enforced) | `400 VALIDATION_ERROR` |
//!
//! The holder of a meeting space is its organization (`ORGANIZATION`) when it has one,
//! else its owner account (`USER`); spaces of a Platform Access application environment are
//! partner usage, not a user plan, and are never limited here.
//!
//! **Read** (signed `cf-service+jwt`, audience `cfmeet.developer-platform`):
//! `GET /internal/v1/plans/holders/{holderKind}/{holderId}/limits` →
//! `{"planCode": "FREE", "limits": {"<key>": <integer> | null}}` where `null` (or an absent
//! key) means unlimited; a boolean limit may also be sent as `true` / `false` (1 / 0).
//! ASSUMED SHAPE: the Developer Platform route is not built yet (its pricing module owns the
//! data, `PLAN_LIMITS` keys `meetingMinutes`, `participants`, `recording`, `storageBytes`,
//! `attachmentBytes`, `webinars`). Answers are cached per holder for
//! `MEETING_PLAN_LIMITS_CACHE_SECS` (default 60).
//!
//! **Fail open.** Off unless `MEETING_PLAN_ENFORCEMENT_ENABLED=true` (and
//! `CF_DEVELOPER_PLATFORM_BASE_URL` plus the service signing identity are configured). When
//! off, unconfigured, unreachable, slow (2 s), or answering anything unexpected, nothing
//! here refuses: today's limits stay the only limits. A failed read is cached briefly (10 s)
//! so an outage does not add a call to every join.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `MEETING_PLAN_ENFORCEMENT_ENABLED` | `true` turns enforcement on (default off) |
//! | `CF_DEVELOPER_PLATFORM_BASE_URL` | the Developer Platform's internal base URL |
//! | `MEETING_PLAN_LIMITS_CACHE_SECS` | cache lifetime of an answer (default 60, 5–3600) |

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};
use serde_json::Value;
use uuid::Uuid;

use crate::models::meeting_spaces;

/// Who a plan belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Holder {
    User(Uuid),
    Organization(Uuid),
}

impl Holder {
    /// The plan holder of a meeting space (`None`: a partner environment's space, or no
    /// owner at all).
    pub fn of_space(space: &meeting_spaces::Model) -> Option<Self> {
        Self::from_owner(
            space.platform_environment_id,
            space.organization_id,
            space.owner_user_id,
        )
    }

    fn from_owner(
        environment: Option<Uuid>,
        organization: Option<Uuid>,
        owner: Option<Uuid>,
    ) -> Option<Self> {
        if environment.is_some() {
            return None;
        }
        organization
            .map(Holder::Organization)
            .or(owner.map(Holder::User))
    }

    pub fn kind(self) -> &'static str {
        match self {
            Holder::User(_) => "USER",
            Holder::Organization(_) => "ORGANIZATION",
        }
    }

    pub fn id(self) -> Uuid {
        match self {
            Holder::User(id) | Holder::Organization(id) => id,
        }
    }
}

/// A holder's effective limits (`None`: unlimited).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLimits {
    pub plan_code: String,
    limits: HashMap<String, Option<i64>>,
}

impl PlanLimits {
    /// Parses the plan read's answer; `None` for anything unexpected (fail open).
    pub fn from_json(v: &Value) -> Option<Self> {
        let plan_code = v.get("planCode")?.as_str()?.trim();
        if plan_code.is_empty() || plan_code.len() > 64 {
            return None;
        }
        let mut limits = HashMap::new();
        for (k, value) in v.get("limits")?.as_object()? {
            let parsed = match value {
                Value::Null => None,
                Value::Bool(b) => Some(i64::from(*b)),
                Value::Number(n) => Some(n.as_i64().filter(|n| *n >= 0)?),
                _ => return None,
            };
            limits.insert(k.clone(), parsed);
        }
        Some(Self {
            plan_code: plan_code.to_string(),
            limits,
        })
    }

    fn get(&self, key: &str) -> Option<i64> {
        self.limits.get(key).copied().flatten()
    }

    /// Participants per meeting (`participants`).
    pub fn participants(&self) -> Option<u32> {
        self.get("participants")
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Whether recording is included (`recording`: 0 = no; absent / null = yes).
    pub fn recording_allowed(&self) -> bool {
        self.get("recording") != Some(0)
    }

    /// The longest a meeting may run (`meetingMinutes`).
    pub fn meeting_minutes(&self) -> Option<i64> {
        self.get("meetingMinutes")
    }
}

/// Where effective limits come from.
#[async_trait]
pub trait PlanSource: Send + Sync {
    /// `Ok(None)`: the holder has no plan answer (treated as no limit).
    async fn limits(&self, holder: Holder) -> Result<Option<PlanLimits>, String>;
}

/// The Developer Platform's signed plan read.
pub struct DeveloperPlatformPlans {
    client: InternalServiceClient,
}

#[async_trait]
impl PlanSource for DeveloperPlatformPlans {
    async fn limits(&self, holder: Holder) -> Result<Option<PlanLimits>, String> {
        let path = format!(
            "/internal/v1/plans/holders/{}/{}/limits",
            holder.kind(),
            holder.id()
        );
        let r = self
            .client
            .get(
                &OutboundServiceRequest::new_operation(ServiceId::developer_platform()),
                &path,
            )
            .await
            .map_err(|e| format!("plan read failed: {e}"))?;
        match r.status() {
            200 => {
                let v: Value = r
                    .json()
                    .map_err(|_| "plan read: invalid JSON".to_string())?;
                PlanLimits::from_json(&v)
                    .map(Some)
                    .ok_or_else(|| "plan read: unexpected answer".to_string())
            }
            404 => Ok(None),
            s => Err(format!("plan read answered HTTP {s}")),
        }
    }
}

/// A fixed answer per holder (tests and local tooling).
#[derive(Default)]
pub struct StaticPlans(pub Mutex<HashMap<Holder, PlanLimits>>);

#[async_trait]
impl PlanSource for StaticPlans {
    async fn limits(&self, holder: Holder) -> Result<Option<PlanLimits>, String> {
        Ok(self.0.lock().expect("plans").get(&holder).cloned())
    }
}

const FAILURE_TTL: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CACHED: usize = 10_000;

/// Effective limits with a short per-holder cache; every failure is "no plan limit".
pub struct PlanEnforcement {
    source: Arc<dyn PlanSource>,
    ttl: Duration,
    cache: Mutex<HashMap<Holder, (Instant, Option<PlanLimits>)>>,
}

impl PlanEnforcement {
    pub fn new(source: Arc<dyn PlanSource>, ttl: Duration) -> Self {
        Self {
            source,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// From the environment: `Ok(None)` when off or not configured (fail open); `Err` only
    /// for a configuration that is on but malformed.
    pub fn from_env() -> Result<Option<Self>, String> {
        if !crate::config::env_bool("MEETING_PLAN_ENFORCEMENT_ENABLED", false) {
            return Ok(None);
        }
        let env = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let (Some(url), Some(runtime), Some(kid), Some(key_file)) = (
            env("CF_DEVELOPER_PLATFORM_BASE_URL"),
            env("CF_RUNTIME_ENVIRONMENT"),
            env("CF_SERVICE_AUTH_KEY_ID"),
            env("CF_SERVICE_AUTH_PRIVATE_KEY_FILE"),
        ) else {
            tracing::warn!(
                "plan enforcement is on but the Developer Platform plan read is not configured; \
                 today's limits stay in force (fail open)"
            );
            return Ok(None);
        };
        let runtime = RuntimeEnvironment::parse(&runtime)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&kid).map_err(|_| "CF_SERVICE_AUTH_KEY_ID is invalid")?,
            &PathBuf::from(key_file),
        )
        .map_err(|_| "CF_SERVICE_AUTH_PRIVATE_KEY_FILE is not a readable PKCS#8 Ed25519 key")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid token TTL")?,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(runtime);
        targets
            .insert(ServiceId::developer_platform(), &url)
            .map_err(|e| format!("CF_DEVELOPER_PLATFORM_BASE_URL: {e}"))?;
        let client = InternalServiceClient::new(signer, targets, READ_TIMEOUT)
            .map_err(|e| format!("plan read client: {e}"))?;
        let ttl = env("MEETING_PLAN_LIMITS_CACHE_SECS")
            .map(|v| {
                v.parse::<u64>()
                    .ok()
                    .filter(|s| (5..=3600).contains(s))
                    .ok_or("MEETING_PLAN_LIMITS_CACHE_SECS must be 5-3600")
            })
            .transpose()?
            .unwrap_or(60);
        Ok(Some(Self::new(
            Arc::new(DeveloperPlatformPlans { client }),
            Duration::from_secs(ttl),
        )))
    }

    /// The holder's effective limits (`None`: no limit to apply, including every failure).
    pub async fn effective(&self, holder: Holder) -> Option<PlanLimits> {
        let now = Instant::now();
        if let Some((until, cached)) = self.cache.lock().expect("plan cache").get(&holder) {
            if *until > now {
                return cached.clone();
            }
        }
        let (answer, ttl) =
            match tokio::time::timeout(READ_TIMEOUT, self.source.limits(holder)).await {
                Ok(Ok(limits)) => (limits, self.ttl),
                Ok(Err(e)) => {
                    tracing::warn!(
                        holder = holder.kind(),
                        "plan limits unavailable, not enforced: {e}"
                    );
                    (None, FAILURE_TTL)
                }
                Err(_) => {
                    tracing::warn!(
                        holder = holder.kind(),
                        "plan limits read timed out, not enforced"
                    );
                    (None, FAILURE_TTL)
                }
            };
        let mut cache = self.cache.lock().expect("plan cache");
        if cache.len() >= MAX_CACHED {
            cache.retain(|_, (until, _)| *until > now);
            if cache.len() >= MAX_CACHED {
                cache.clear();
            }
        }
        cache.insert(holder, (now + ttl, answer.clone()));
        answer
    }

    /// The effective limits of a meeting space's holder.
    pub async fn for_space(&self, space: &meeting_spaces::Model) -> Option<PlanLimits> {
        self.effective(Holder::of_space(space)?).await
    }
}

/// The effective limits of `space`'s holder when enforcement is on (`None` otherwise).
pub async fn for_space(
    enforcement: &Option<Arc<PlanEnforcement>>,
    space: &meeting_spaces::Model,
) -> Option<PlanLimits> {
    match enforcement {
        Some(e) => e.for_space(space).await,
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn answers_parse_and_unexpected_ones_fail_open() {
        let l = PlanLimits::from_json(&json!({
            "planCode": "FREE",
            "limits": { "participants": 100, "recording": false, "meetingMinutes": 40, "webinars": null }
        }))
        .unwrap();
        assert_eq!(l.plan_code, "FREE");
        assert_eq!(l.participants(), Some(100));
        assert!(!l.recording_allowed());
        assert_eq!(l.meeting_minutes(), Some(40));
        let pro = PlanLimits::from_json(&json!({
            "planCode": "PRO", "limits": { "recording": 1 }
        }))
        .unwrap();
        assert_eq!(pro.participants(), None);
        assert!(pro.recording_allowed());
        assert_eq!(pro.meeting_minutes(), None);
        for bad in [
            json!({}),
            json!({ "planCode": "", "limits": {} }),
            json!({ "planCode": "FREE" }),
            json!({ "planCode": "FREE", "limits": { "participants": -1 } }),
            json!({ "planCode": "FREE", "limits": { "participants": "100" } }),
            json!({ "planCode": "FREE", "limits": [] }),
        ] {
            assert!(PlanLimits::from_json(&bad).is_none(), "{bad}");
        }
    }

    struct Counting(AtomicUsize, Result<Option<PlanLimits>, String>);

    #[async_trait]
    impl PlanSource for Counting {
        async fn limits(&self, _: Holder) -> Result<Option<PlanLimits>, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            self.1.clone()
        }
    }

    #[tokio::test]
    async fn answers_are_cached_and_failures_enforce_nothing() {
        let free = PlanLimits::from_json(&json!({
            "planCode": "FREE", "limits": { "participants": 3 }
        }))
        .unwrap();
        let source = Arc::new(Counting(AtomicUsize::new(0), Ok(Some(free.clone()))));
        let e = PlanEnforcement::new(source.clone(), Duration::from_secs(60));
        let h = Holder::User(Uuid::new_v4());
        assert_eq!(e.effective(h).await, Some(free.clone()));
        assert_eq!(e.effective(h).await, Some(free));
        assert_eq!(source.0.load(Ordering::SeqCst), 1, "cached");

        let down = Arc::new(Counting(AtomicUsize::new(0), Err("unreachable".into())));
        let e = PlanEnforcement::new(down.clone(), Duration::from_secs(60));
        assert_eq!(e.effective(h).await, None, "fail open");
        assert_eq!(e.effective(h).await, None);
        assert_eq!(
            down.0.load(Ordering::SeqCst),
            1,
            "a failure is cached briefly"
        );
    }

    #[test]
    fn partner_spaces_have_no_user_plan() {
        let (org, owner, env) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            Holder::from_owner(None, Some(org), Some(owner)),
            Some(Holder::Organization(org))
        );
        assert_eq!(
            Holder::from_owner(None, None, Some(owner)),
            Some(Holder::User(owner))
        );
        assert_eq!(Holder::from_owner(Some(env), None, Some(owner)), None);
        assert_eq!(Holder::from_owner(None, None, None), None);
        assert_eq!(Holder::User(owner).kind(), "USER");
        assert_eq!(Holder::Organization(org).id(), org);
    }
}
