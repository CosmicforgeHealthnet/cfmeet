//! Calling's media-provider boundary (Part C §24).
//!
//! * [`provider`] — the call-oriented, provider-neutral contract ([`CallMediaProvider`])
//!   business code depends on, with generic capability flags;
//! * [`permissions`] — server-authoritative media permissions and the hold policy;
//! * [`credentials`] — short-lived join-credential policy and the safe public view;
//! * [`events`] — admission decisions for provider callbacks (evidence only);
//! * [`reconciliation`] — the pure diff between Calling's authoritative sessions and what
//!   the provider reports, with the safe repairs it allows;
//! * [`gateway`] — CF Meet's admission authority over every media signaling connection
//!   (provider-neutral; fail closed);
//! * [`livekit`] / [`livekit_gateway`] — the self-hosted LiveKit adapter and its
//!   connection gateway, the only provider-specific code.
//!
//! This module is independent of the Meeting Service: it shares no source, type, HTTP API
//! or database with it.

pub mod credentials;
pub mod events;
pub mod gateway;
pub mod livekit;
pub mod livekit_gateway;
pub mod permissions;
pub mod provider;
pub mod reconciliation;

use std::sync::Arc;

pub use credentials::{MediaCredentialPolicy, MediaCredentialView};
pub use permissions::{MediaPermissions, MEDIA_POLICY_VERSION};
pub use provider::{
    CallMediaProvider, MediaProviderCapabilities, MediaProviderError, MediaResourceName,
};

use crate::config::ConfigError;

/// Which media provider executes application RTC Calls.
pub enum MediaProviderSelection {
    /// No media provider: application calls are refused with CAPABILITY_UNAVAILABLE.
    Disabled,
    /// The self-hosted LiveKit deployment.
    LiveKit(livekit::LiveKitSettings),
}

/// Media configuration of the Calling service.
pub struct MediaSettings {
    pub selection: MediaProviderSelection,
    pub credential_policy: MediaCredentialPolicy,
}

impl MediaSettings {
    /// `CALLING_MEDIA_PROVIDER` = `livekit` | `none` (default `none`). Selecting a provider
    /// requires its complete configuration: there is no fallback credential, and an
    /// invalid configuration stops the service.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let credential_policy = MediaCredentialPolicy::from_lookup(get)?;
        let selection = match get("CALLING_MEDIA_PROVIDER")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .as_deref()
        {
            None | Some("none") => MediaProviderSelection::Disabled,
            Some("livekit") => {
                MediaProviderSelection::LiveKit(livekit::LiveKitSettings::from_lookup(get)?)
            }
            Some(_) => {
                return Err(ConfigError::invalid(
                    "CALLING_MEDIA_PROVIDER",
                    "must be `livekit` or `none`".to_string(),
                ))
            }
        };
        Ok(Self {
            selection,
            credential_policy,
        })
    }

    /// Builds the configured provider and its connection gateway (`None` when media is
    /// disabled).
    pub fn build(&self) -> Result<MediaRuntime, ConfigError> {
        Ok(match &self.selection {
            MediaProviderSelection::Disabled => MediaRuntime {
                provider: None,
                gateway: None,
                public_bridge: None,
            },
            MediaProviderSelection::LiveKit(settings) => {
                let adapter = Arc::new(livekit::LiveKitAdapter::new(settings.clone_settings())?);
                let public_bridge: Option<Arc<dyn crate::telephony::media::PublicMediaGateway>> =
                    settings.sip_trunk_id().map(|_| {
                        Arc::new(livekit::LiveKitSipGateway::new(adapter.clone()))
                            as Arc<dyn crate::telephony::media::PublicMediaGateway>
                    });
                MediaRuntime {
                    provider: Some(adapter.clone()),
                    gateway: Some(Arc::new(livekit_gateway::LiveKitGateway::new(
                        adapter.clone(),
                    ))),
                    public_bridge,
                }
            }
        })
    }
}

/// A configured media provider and the gateway every media connection passes through.
pub struct MediaRuntime {
    pub provider: Option<Arc<dyn CallMediaProvider>>,
    pub gateway: Option<Arc<dyn gateway::MediaConnectionGateway>>,
    /// Attaches public-network bridges to application media (Part E §79).
    pub public_bridge: Option<Arc<dyn crate::telephony::media::PublicMediaGateway>>,
}
