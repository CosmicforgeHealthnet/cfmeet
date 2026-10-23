//! Bounded authorization caching (Part V B.7).
//!
//! A positive authentication is reused ONLY for read routes, ONLY for `ttl` (≤ 60 s) and
//! ONLY while the identity change-feed watcher is healthy:
//!
//! * the watcher polls Platform Access's commit-ordered change feed
//!   (`POST /internal/v1/identity/changes`, `platform-identity.read`); ANY new change
//!   (credential revoked, scope withdrawn, tenant suspended, principal deactivated, …)
//!   flushes the whole cache;
//! * a failed poll marks the watcher unhealthy, flushes the cache and disables caching
//!   until a poll succeeds again — Platform Access being unreachable is never an offline
//!   authentication bypass (fresh authentication then fails closed as well);
//! * entries are keyed by SHA-256 of the presented credential (the value is never kept)
//!   and the cache is bounded (flushed when full).
//!
//! Requests forwarded by the API Gateway ([`CustomerAuthority::authenticate_forwarded`])
//! are never cached: each one re-fetches the credential's chain from Platform Access.
//!
//! Mutations and security-critical operations (device registration, KeyPackage
//! publication, revocation, conversation creation / lifecycle) always authenticate
//! afresh, and the acting principal is always resolved afresh.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use cf_service_auth::{client::InternalServiceClient, services, OutboundServiceRequest, ServiceId};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Authentication, AuthorityUnavailable, CustomerAuthority};

/// Largest number of cached authentications.
pub const MAX_ENTRIES: usize = 10_000;

/// Freshness required by a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Always ask Platform Access.
    Fresh,
    /// A bounded, feed-invalidated cached answer is acceptable.
    BoundedStale,
}

/// One page of the identity change feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedPage {
    pub changes: usize,
    pub next_after: i64,
    pub has_more: bool,
}

#[async_trait::async_trait]
pub trait IdentityFeed: Send + Sync {
    async fn page(&self, after: i64) -> Result<FeedPage, AuthorityUnavailable>;
}

/// The Platform Access change feed.
pub struct PlatformAccessFeed {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessFeed {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl IdentityFeed for PlatformAccessFeed {
    async fn page(&self, after: i64) -> Result<FeedPage, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(
            ServiceId::parse(services::PLATFORM_ACCESS).expect("canonical identity"),
        );
        let response = self
            .client
            .post_json(
                &request,
                "/internal/v1/identity/changes",
                &json!({ "after": after, "limit": 500 }),
            )
            .await
            .map_err(|e| AuthorityUnavailable(e.to_string()))?;
        if response.status() != 200 {
            return Err(AuthorityUnavailable(format!("HTTP {}", response.status())));
        }
        let body: Value = response
            .json()
            .map_err(|_| AuthorityUnavailable("malformed change page".into()))?;
        Ok(FeedPage {
            changes: body["changes"]
                .as_array()
                .map(Vec::len)
                .ok_or_else(|| AuthorityUnavailable("malformed change page".into()))?,
            next_after: body["nextAfter"]
                .as_i64()
                .ok_or_else(|| AuthorityUnavailable("malformed change page".into()))?,
            has_more: body["hasMore"].as_bool().unwrap_or(false),
        })
    }
}

/// A [`CustomerAuthority`] with a bounded, invalidated positive cache.
pub struct CachingAuthority {
    inner: Arc<dyn CustomerAuthority>,
    ttl: Duration,
    healthy: AtomicBool,
    entries: Mutex<HashMap<[u8; 32], (super::TrustedContext, Instant)>>,
}

impl CachingAuthority {
    /// `ttl` zero disables caching entirely. Caching starts disabled until the watcher
    /// has caught up with the feed ([`Self::watch`]).
    pub fn new(inner: Arc<dyn CustomerAuthority>, ttl: Duration) -> Self {
        Self {
            inner,
            ttl: ttl.min(Duration::from_secs(60)),
            healthy: AtomicBool::new(false),
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn key(presented: &str) -> [u8; 32] {
        Sha256::digest(presented.as_bytes()).into()
    }

    fn entries(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], (super::TrustedContext, Instant)>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn flush(&self) {
        self.entries().clear();
    }

    pub fn is_caching(&self) -> bool {
        !self.ttl.is_zero() && self.healthy.load(Ordering::SeqCst)
    }

    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Marks the feed healthy / unhealthy; unhealthy flushes.
    pub fn set_healthy(&self, healthy: bool) {
        self.healthy.store(healthy, Ordering::SeqCst);
        if !healthy {
            self.flush();
        }
    }

    pub async fn authenticate_with(
        &self,
        presented: &str,
        freshness: Freshness,
    ) -> Result<Authentication, AuthorityUnavailable> {
        let key = Self::key(presented);
        if freshness == Freshness::BoundedStale && self.is_caching() {
            if let Some((ctx, at)) = self.entries().get(&key).cloned() {
                if at.elapsed() < self.ttl {
                    crate::metrics::inc("messaging_auth_cache_total", &[("outcome", "hit")]);
                    return Ok(Authentication::Authenticated(Box::new(ctx)));
                }
            }
        }
        let outcome = self.inner.authenticate(presented).await;
        match &outcome {
            Ok(Authentication::Authenticated(ctx)) if self.is_caching() => {
                let mut entries = self.entries();
                if entries.len() >= MAX_ENTRIES {
                    entries.clear();
                }
                entries.insert(key, ((**ctx).clone(), Instant::now()));
            }
            // A rejection or an outage evicts any cached positive answer at once.
            _ => {
                self.entries().remove(&key);
            }
        }
        outcome
    }

    /// One watcher pass from `after`: reads to the end of the feed; returns the new
    /// cursor. Any change flushes; a failure marks the cache unhealthy.
    pub async fn poll(&self, feed: &dyn IdentityFeed, mut after: i64) -> i64 {
        loop {
            match feed.page(after).await {
                Ok(page) => {
                    if page.changes > 0 {
                        self.flush();
                        crate::metrics::inc("messaging_auth_cache_flushes_total", &[]);
                    }
                    after = page.next_after.max(after);
                    if !page.has_more {
                        self.set_healthy(true);
                        return after;
                    }
                }
                Err(e) => {
                    if self.healthy.load(Ordering::SeqCst) {
                        tracing::warn!(error = %e, "identity change feed unavailable: authorization caching disabled");
                    }
                    self.set_healthy(false);
                    return after;
                }
            }
        }
    }

    /// Runs the watcher until the process stops.
    pub async fn watch(self: Arc<Self>, feed: Arc<dyn IdentityFeed>, interval: Duration) {
        let mut after = 0;
        loop {
            after = self.poll(feed.as_ref(), after).await;
            tokio::time::sleep(interval).await;
        }
    }
}

#[async_trait::async_trait]
impl CustomerAuthority for CachingAuthority {
    /// The trait entry point is always FRESH.
    async fn authenticate(&self, presented: &str) -> Result<Authentication, AuthorityUnavailable> {
        self.authenticate_with(presented, Freshness::Fresh).await
    }

    /// Always fresh: an API Gateway forwarded request is never answered from the cache
    /// (revocation and suspension apply at once).
    async fn authenticate_forwarded(
        &self,
        ctx: &cf_service_auth::DelegatedPlatformContext,
        correlation_id: &cf_service_auth::CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        self.inner.authenticate_forwarded(ctx, correlation_id).await
    }

    /// Always fresh (internal delegated calls are few and change state).
    async fn validate_delegated(
        &self,
        ctx: &cf_service_auth::DelegatedPlatformContext,
    ) -> Result<super::DelegatedScope, AuthorityUnavailable> {
        self.inner.validate_delegated(ctx).await
    }
}
