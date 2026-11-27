//! The provider boundary every push transport implements, moved verbatim from Messaging
//! (`services/messaging/src/realtime/push.rs`, roadmap N4). Messaging re-exports these
//! items under their old paths, so its behaviour and tests are unchanged.

use std::sync::Mutex;

use serde_json::Value;

/// Outcome of one provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    Sent,
    Retryable(String),
    /// The provider refused the handle for good (unregistered app, bad token).
    InvalidHandle,
    Permanent(String),
}

/// Per-send delivery options (Notification Service roadmap N9). The defaults are exactly
/// the behaviour of [`PushProvider::send`]: the 24 h TTL ([`crate::adapters::TTL_SECONDS`]),
/// no collapse identifier and a regular (alert / data) push. Message wake-ups use the
/// defaults (parity with Messaging); the call fast lane (I3) shortens the TTL to the
/// invitation's expiry, collapses repeated call alerts and sends APNs VoIP pushes to
/// `APNS_VOIP` registrations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendOptions {
    /// Seconds the provider may hold the push (`None`: the 24 h default; never more).
    /// `0` means "now or never".
    pub ttl_seconds: Option<u64>,
    /// Replaces an earlier push with the same id that is still waiting or displayed (Web
    /// Push `Topic`, APNs `apns-collapse-id`, FCM `collapse_key`). At most 32 characters of
    /// the URL-safe base64 alphabet (the strictest provider, RFC 8030 §5.4); anything else
    /// is ignored rather than sent.
    pub collapse_id: Option<String>,
    /// APNs only: a PushKit VoIP push (`apns-push-type: voip`, topic `<bundle id>.voip`).
    pub voip: bool,
}

impl SendOptions {
    /// The collapse id when it is safe for every provider (≤ 32 URL-safe base64 characters).
    pub fn safe_collapse_id(&self) -> Option<&str> {
        self.collapse_id.as_deref().filter(|c| {
            !c.is_empty()
                && c.len() <= 32
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
    }

    /// The TTL to send, in seconds (never above the 24 h default).
    pub fn ttl(&self) -> u64 {
        self.ttl_seconds
            .unwrap_or(crate::adapters::TTL_SECONDS)
            .min(crate::adapters::TTL_SECONDS)
    }
}

#[async_trait::async_trait]
pub trait PushProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Whether this provider can deliver to `platform` in this deployment.
    fn supports(&self, platform: &str) -> bool;
    /// Provider-specific checks of a handle at registration (e.g. the Web Push host
    /// allowlist); structural checks run regardless.
    fn validate_handle(&self, _platform: &str, _handle: &str) -> Result<(), &'static str> {
        Ok(())
    }
    async fn send(&self, platform: &str, handle: &str, payload: &Value) -> PushOutcome;
    /// [`send`](Self::send) with delivery options. A provider without options support
    /// sends exactly as [`send`](Self::send) does.
    async fn send_with(
        &self,
        platform: &str,
        handle: &str,
        payload: &Value,
        _options: &SendOptions,
    ) -> PushOutcome {
        self.send(platform, handle, payload).await
    }
    /// The Web Push application-server (VAPID) public key clients pass to
    /// `pushManager.subscribe` (base64url, uncompressed P-256 point); `None` when Web Push is
    /// not configured. Public by nature; the private key never leaves the adapter.
    fn web_push_application_server_key(&self) -> Option<String> {
        None
    }
}

/// A LOCAL adapter (tests / development only; refused in staging and production by the
/// configuration): records what would have been sent to `LOCAL_TEST` registrations.
#[derive(Default)]
pub struct LocalTestPush {
    pub sent: Mutex<Vec<(String, Value)>>,
    pub fail_next: Mutex<u32>,
    /// The delivery options of each [`PushProvider::send_with`] call, in order.
    pub options: Mutex<Vec<SendOptions>>,
}

#[async_trait::async_trait]
impl PushProvider for LocalTestPush {
    fn name(&self) -> &'static str {
        "local-test"
    }

    fn supports(&self, platform: &str) -> bool {
        platform == "LOCAL_TEST"
    }

    async fn send(&self, _platform: &str, handle: &str, payload: &Value) -> PushOutcome {
        let mut fail = self.fail_next.lock().unwrap();
        if *fail > 0 {
            *fail -= 1;
            return PushOutcome::Retryable("injected failure".into());
        }
        self.sent
            .lock()
            .unwrap()
            .push((handle.to_string(), payload.clone()));
        PushOutcome::Sent
    }

    async fn send_with(
        &self,
        platform: &str,
        handle: &str,
        payload: &Value,
        options: &SendOptions,
    ) -> PushOutcome {
        self.options.lock().unwrap().push(options.clone());
        self.send(platform, handle, payload).await
    }
}
