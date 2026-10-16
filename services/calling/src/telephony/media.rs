//! Application ⇄ public-network media bridge (Part E §79).
//!
//! Telephony anchors the public leg on the SIP / media edge and issues an opaque,
//! single-use MEDIA JOIN CODE for an APPLICATION_MEDIA bridge. Calling attaches its RTC
//! resource to that bridge through this port (implemented by the media provider adapter
//! in `crate::media`): the media provider adds a server-side
//! participant (reserved identity `cfm-pstn-<bridge>`, admitted only for that bridge)
//! that dials the edge with the code. The application participant never learns the
//! private destination, and no SIP type reaches Calling's business code.

use async_trait::async_trait;

use crate::media::MediaProviderError;

#[async_trait]
pub trait PublicMediaGateway: Send + Sync {
    /// Attaches the edge bridge identified by `join_code` to `room` as `identity`.
    async fn attach(
        &self,
        room: &str,
        identity: &str,
        join_code: &str,
    ) -> Result<(), MediaProviderError>;
}

/// The reserved media identity of a public bridge (`cfm-pstn-<32 hex>`).
pub fn bridge_identity(bridge_id: uuid::Uuid) -> String {
    format!("cfm-pstn-{}", bridge_id.simple())
}

/// Whether `identity` is a reserved public-bridge identity (never a session identity).
pub fn is_bridge_identity(identity: &str) -> bool {
    identity
        .strip_prefix("cfm-pstn-")
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}
