//! Push-ready ring offers (Part C §88).
//!
//! No mobile push vendor is integrated. When an offer starts ringing an endpoint that
//! registered a `pushTargetRef`, Calling builds the transport-neutral payload of contract
//! `cfmeet.calling.push-invitation` (call id, offer id, call kind, optional caller display
//! name, issue and expiry times) and hands it — after the authoritative transaction
//! committed, like every realtime signal — to the configured [`PushDispatcher`]. The
//! payload never carries a media credential, provider token, API credential or telephone
//! number: the device acts on it with its own signaling credential (recover, then accept
//! or decline).

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

/// A push transport. Dispatch is best effort: a lost push never loses state (the offer is
/// recoverable through `GET /v1/calling/invitations`).
#[async_trait]
pub trait PushDispatcher: Send + Sync + 'static {
    /// Delivers `payload` to the application's push target `push_target_ref`.
    async fn dispatch(&self, push_target_ref: Uuid, payload: Value);
}

/// No push transport configured: offers reach endpoints through signaling and recovery.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPushTransport;

#[async_trait]
impl PushDispatcher for NoPushTransport {
    async fn dispatch(&self, _push_target_ref: Uuid, _payload: Value) {
        tracing::debug!("push-ready offer not dispatched: no push transport is configured");
    }
}
