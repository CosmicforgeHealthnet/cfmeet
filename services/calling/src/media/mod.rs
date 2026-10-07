//! Calling's media-provider boundary (Part C §24).
//!
//! * [`provider`] — the call-oriented, provider-neutral contract ([`CallMediaProvider`])
//!   business code depends on, with generic capability flags;
//! * [`permissions`] — server-authoritative media permissions and the hold policy;
//! * [`credentials`] — short-lived join-credential policy and the safe public view;
//! * [`events`] — admission decisions for provider callbacks (evidence only);
//! * [`reconciliation`] — the pure diff between Calling's authoritative sessions and what
//!   the provider reports, with the safe repairs it allows;
//! * [`livekit`] — the self-hosted LiveKit adapter, the only provider-specific code.
//!
//! This module is independent of the Meeting Service: it shares no source, type, HTTP API
//! or database with it.

pub mod credentials;
pub mod events;
pub mod livekit;
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

    /// Builds the configured provider (`None` when media is disabled).
    pub fn build_provider(&self) -> Result<Option<Arc<dyn CallMediaProvider>>, ConfigError> {
        Ok(match &self.selection {
            MediaProviderSelection::Disabled => None,
            MediaProviderSelection::LiveKit(settings) => Some(Arc::new(
                livekit::LiveKitAdapter::new(settings.clone_settings())?,
            )),
        })
    }
}
