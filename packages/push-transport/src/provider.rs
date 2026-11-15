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
}
