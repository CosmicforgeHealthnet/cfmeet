//! Asterisk ARI adapter of [`super::SipMediaEdge`] (Part E §41). Implemented in Stage 3.

use super::EdgeConfigError;

pub struct AriSettings;

impl AriSettings {
    pub fn from_env(_: cf_service_auth::RuntimeEnvironment) -> Result<Self, EdgeConfigError> {
        Err(EdgeConfigError::Missing("TELEPHONY_ARI_URL"))
    }
}

pub struct AriEdge;

impl AriEdge {
    pub fn start(_: AriSettings) -> super::NoEdge {
        super::NoEdge::default()
    }
}
