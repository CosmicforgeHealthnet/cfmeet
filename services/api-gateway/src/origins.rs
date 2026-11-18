//! Partner-website origins (ADR 0062 §4, build contract §2.3 / §9).
//!
//! With `API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED`, the gateway keeps an in-memory index
//! origin → environments, built from Platform Access's change feed
//! `GET /internal/v1/environment-origins?after=<seq>&limit=500`
//! (`platform-environment-origins.read`; one item per change carrying the environment's
//! LATEST state). The feed is polled every 30 seconds or less; each poll pages until the
//! feed is drained. Only environments whose status is `ACTIVE` contribute origins.
//!
//! The index is used by the CORS layer ([`crate::http`]):
//!
//! * **preflight**: a partner origin is allowed only on routes that accept a device
//!   authentication mode, and only when it is registered for some ACTIVE environment;
//! * **actual request**: `Access-Control-Allow-Origin` is set only when the origin is
//!   registered for the environment of the request's device token.
//!
//! Before the first successful sync the index is empty: partner origins are refused
//! (fail closed). A failed poll keeps the last known state. The gateway stores nothing.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, RwLock},
    time::Duration,
};

use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use serde::{Deserialize, Deserializer};
use uuid::Uuid;

pub const FEED_PATH: &str = "/internal/v1/environment-origins";
/// Page size of one feed request (contract: at most 500).
pub const FEED_PAGE_LIMIT: u32 = 500;
/// Poll period of the feed (contract §9: at most 30 seconds).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Bound of one sync (pages), so a misbehaving feed cannot pin the poller.
const MAX_PAGES_PER_SYNC: usize = 1_000;
/// Contract §2.3: at most 20 origins per environment.
const MAX_ORIGINS_PER_ENVIRONMENT: usize = 20;

fn seq<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        Num(u64),
        Str(String),
    }
    match N::deserialize(d)? {
        N::Num(n) => Ok(n),
        N::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

fn opt_seq<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    struct W(#[serde(deserialize_with = "seq")] u64);
    Ok(Option::<W>::deserialize(d)?.map(|w| w.0))
}

/// One change-feed item: an environment's latest allowed origins and status.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginFeedItem {
    pub environment_id: Uuid,
    #[serde(default)]
    pub origins: Vec<String>,
    pub environment_status: String,
    #[serde(deserialize_with = "seq")]
    pub seq: u64,
}

/// One page of the change feed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginFeedPage {
    pub items: Vec<OriginFeedItem>,
    #[serde(default, deserialize_with = "opt_seq")]
    pub next_after: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
#[error("the environment-origins feed is unavailable: {0}")]
pub struct FeedUnavailable(pub String);

/// Where the feed comes from (Platform Access; scripted in tests).
#[async_trait::async_trait]
pub trait OriginFeedSource: Send + Sync {
    async fn page(&self, after: u64, limit: u32) -> Result<OriginFeedPage, FeedUnavailable>;
}

/// Platform Access over authenticated internal HTTP (`platform-environment-origins.read`).
pub struct PlatformAccessOriginFeed {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessOriginFeed {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl OriginFeedSource for PlatformAccessOriginFeed {
    async fn page(&self, after: u64, limit: u32) -> Result<OriginFeedPage, FeedUnavailable> {
        let request = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        let (after, limit) = (after.to_string(), limit.to_string());
        let response = self
            .client
            .get_query(
                &request,
                FEED_PATH,
                &[("after", after.as_str()), ("limit", limit.as_str())],
            )
            .await
            .map_err(|e| FeedUnavailable(e.to_string()))?;
        response
            .json::<OriginFeedPage>()
            .map_err(|_| FeedUnavailable("malformed feed page".into()))
    }
}

/// The canonical form of an exact origin `scheme://host[:port]` (lower case; `https`, or
/// `http` for loopback hosts only), or `None`. Applied to feed values (defence in depth:
/// Platform Access validates them on write) and to presented `Origin` headers.
pub fn canonical_origin(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.len() > 256 || lower.ends_with('/') || lower.contains('*') {
        return None;
    }
    let url = url::Url::parse(&lower).ok()?;
    let host = url.host_str()?;
    let loopback = matches!(host, "localhost" | "127.0.0.1");
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    // The ASCII serialization of the origin must be exactly what was given.
    let serialized = url.origin().ascii_serialization();
    (serialized == lower).then_some(serialized)
}

#[derive(Debug, Clone)]
struct EnvironmentEntry {
    origins: Vec<String>,
    active: bool,
    seq: u64,
}

#[derive(Debug, Default)]
struct IndexState {
    cursor: u64,
    environments: HashMap<Uuid, EnvironmentEntry>,
    /// Derived: origin → ACTIVE environments registering it.
    by_origin: HashMap<String, BTreeSet<Uuid>>,
    synced: bool,
}

impl IndexState {
    fn apply(&mut self, item: OriginFeedItem) {
        if self
            .environments
            .get(&item.environment_id)
            .is_some_and(|e| e.seq > item.seq)
        {
            return;
        }
        let mut origins: Vec<String> = item
            .origins
            .iter()
            .filter_map(|o| canonical_origin(o))
            .collect();
        origins.sort();
        origins.dedup();
        origins.truncate(MAX_ORIGINS_PER_ENVIRONMENT);
        self.environments.insert(
            item.environment_id,
            EnvironmentEntry {
                origins,
                active: item.environment_status == "ACTIVE",
                seq: item.seq,
            },
        );
    }

    fn rebuild(&mut self) {
        let mut by_origin: HashMap<String, BTreeSet<Uuid>> = HashMap::new();
        for (env, entry) in &self.environments {
            if entry.active {
                for o in &entry.origins {
                    by_origin.entry(o.clone()).or_default().insert(*env);
                }
            }
        }
        self.by_origin = by_origin;
    }
}

/// The origin → environments index, rebuilt from the change feed.
pub struct OriginIndex {
    source: Arc<dyn OriginFeedSource>,
    state: RwLock<IndexState>,
    syncing: tokio::sync::Mutex<()>,
    poll_interval: Duration,
}

impl OriginIndex {
    pub fn new(source: Arc<dyn OriginFeedSource>) -> Self {
        Self {
            source,
            state: RwLock::new(IndexState::default()),
            syncing: tokio::sync::Mutex::new(()),
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// Overrides the poll period (capped at 30 seconds).
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.min(DEFAULT_POLL_INTERVAL);
        self
    }

    /// Whether at least one sync completed.
    pub fn synced(&self) -> bool {
        self.state.read().expect("origin index lock").synced
    }

    /// Whether `origin` is registered for some ACTIVE environment.
    pub fn registered(&self, origin: &str) -> bool {
        let Some(origin) = canonical_origin(origin) else {
            return false;
        };
        self.state
            .read()
            .expect("origin index lock")
            .by_origin
            .get(&origin)
            .is_some_and(|envs| !envs.is_empty())
    }

    /// Whether `origin` is registered for `environment` (which must be ACTIVE).
    pub fn allows(&self, origin: &str, environment: Uuid) -> bool {
        let Some(origin) = canonical_origin(origin) else {
            return false;
        };
        self.state
            .read()
            .expect("origin index lock")
            .by_origin
            .get(&origin)
            .is_some_and(|envs| envs.contains(&environment))
    }

    /// Drains the feed from the current cursor. Returns the number of items applied, or
    /// the error of the first failing page (items of earlier pages stay applied).
    pub async fn sync(&self) -> Result<usize, FeedUnavailable> {
        let _guard = self.syncing.lock().await;
        let mut applied = 0;
        for _ in 0..MAX_PAGES_PER_SYNC {
            let after = self.state.read().expect("origin index lock").cursor;
            let page = self.source.page(after, FEED_PAGE_LIMIT).await?;
            let count = page.items.len();
            let max_seq = page.items.iter().map(|i| i.seq).max();
            let next = page.next_after.or(max_seq).unwrap_or(after).max(after);
            {
                let mut state = self.state.write().expect("origin index lock");
                for item in page.items {
                    state.apply(item);
                }
                state.cursor = next;
                state.rebuild();
                state.synced = true;
            }
            applied += count;
            if count < FEED_PAGE_LIMIT as usize || next == after {
                break;
            }
        }
        Ok(applied)
    }

    /// Background poll (every `poll_interval`).
    pub async fn run_poller(self: Arc<Self>) {
        loop {
            if let Err(e) = self.sync().await {
                tracing::warn!(error = %e, "environment origins could not be refreshed");
            }
            tokio::time::sleep(self.poll_interval.max(Duration::from_millis(100))).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn origins_are_exact_and_canonical() {
        for (given, want) in [
            ("https://shop.example.com", Some("https://shop.example.com")),
            ("HTTPS://Shop.Example.com", Some("https://shop.example.com")),
            (
                "https://shop.example.com:8443",
                Some("https://shop.example.com:8443"),
            ),
            ("http://localhost:3000", Some("http://localhost:3000")),
            ("http://127.0.0.1:5173", Some("http://127.0.0.1:5173")),
            ("http://shop.example.com", None),
            ("https://shop.example.com/", None),
            ("https://shop.example.com/app", None),
            ("https://*.example.com", None),
            ("https://u:p@shop.example.com", None),
            ("https://shop.example.com:443", None),
            ("null", None),
            ("", None),
        ] {
            assert_eq!(canonical_origin(given).as_deref(), want, "{given}");
        }
    }

    struct Feed {
        items: Mutex<Vec<OriginFeedItem>>,
        calls: Mutex<Vec<u64>>,
    }

    #[async_trait::async_trait]
    impl OriginFeedSource for Feed {
        async fn page(&self, after: u64, limit: u32) -> Result<OriginFeedPage, FeedUnavailable> {
            self.calls.lock().unwrap().push(after);
            let items: Vec<_> = self
                .items
                .lock()
                .unwrap()
                .iter()
                .filter(|i| i.seq > after)
                .take(limit as usize)
                .cloned()
                .collect();
            let next_after = items.last().map(|i| i.seq);
            Ok(OriginFeedPage { items, next_after })
        }
    }

    fn item(env: Uuid, origins: &[&str], status: &str, seq: u64) -> OriginFeedItem {
        OriginFeedItem {
            environment_id: env,
            origins: origins.iter().map(|s| s.to_string()).collect(),
            environment_status: status.into(),
            seq,
        }
    }

    #[tokio::test]
    async fn the_feed_builds_and_updates_the_index() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut items = vec![
            item(a, &["https://a.example"], "ACTIVE", 1),
            item(
                b,
                &["https://b.example", "https://shared.example"],
                "ACTIVE",
                2,
            ),
        ];
        // Enough items to need several pages.
        for n in 0..1200u64 {
            items.push(item(Uuid::new_v4(), &[], "ACTIVE", 3 + n));
        }
        let feed = Arc::new(Feed {
            items: Mutex::new(items),
            calls: Mutex::default(),
        });
        let index = OriginIndex::new(feed.clone());
        assert!(
            !index.registered("https://a.example"),
            "fail closed before sync"
        );
        assert_eq!(index.sync().await.unwrap(), 1202);
        assert!(feed.calls.lock().unwrap().len() >= 3, "paginated");
        assert!(index.allows("https://a.example", a));
        assert!(!index.allows("https://a.example", b));
        assert!(index.registered("https://shared.example"));
        assert!(!index.registered("https://unknown.example"));
        // Changes: b suspended, a's origins replaced; a stale item never wins.
        {
            let mut items = feed.items.lock().unwrap();
            items.push(item(b, &["https://b.example"], "SUSPENDED", 5000));
            items.push(item(a, &["https://a2.example"], "ACTIVE", 5001));
        }
        assert_eq!(index.sync().await.unwrap(), 2);
        assert!(!index.registered("https://b.example"));
        assert!(!index.allows("https://a.example", a));
        assert!(index.allows("https://a2.example", a));
        assert_eq!(*feed.calls.lock().unwrap().last().unwrap(), 5001);
        let mut state = index.state.write().unwrap();
        state.apply(item(a, &["https://old.example"], "ACTIVE", 7));
        state.rebuild();
        assert!(state.by_origin.contains_key("https://a2.example"));
    }

    #[test]
    fn feed_pages_parse_numbers_and_strings() {
        let page: OriginFeedPage = serde_json::from_str(
            r#"{"items":[{"environmentId":"6f1c2c55-0e7a-4f37-9d65-0a5d5bb1c0de","origins":["https://x.example"],"environmentStatus":"ACTIVE","seq":"42"}],"nextAfter":42}"#,
        )
        .unwrap();
        assert_eq!((page.items[0].seq, page.next_after), (42, Some(42)));
        let empty: OriginFeedPage =
            serde_json::from_str(r#"{"items":[],"nextAfter":null}"#).unwrap();
        assert_eq!(empty.next_after, None);
    }
}
