//! SIP / media edge boundary (Part E §41, §45–§53).
//!
//! [`SipMediaEdge`] is the only way Telephony drives the self-operated SIP / media edge:
//! originate a SIP leg, accept / ring / reject an inbound dialog, hang up, bridge and
//! unbridge (media anchored at the edge), hold / resume, send DTMF, redirect (the
//! transfer primitive), query and list channels, health. Stack-specific control-plane
//! types (the Asterisk ARI adapter in [`ari`]) never leave this directory; business code
//! sees only these neutral types.
//!
//! Every edge channel is correlated to Telephony state through channel variables set at
//! creation (correlation key / media join code); SIP Call-IDs and channel ids stay inside
//! Telephony.

pub mod ari;

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

use crate::{
    domain::{EndReason, Health, LegStatus},
    providers::ExecutionNumber,
};

/// An edge channel reference (internal; never leaves Telephony).
pub type ChannelRef = String;

/// What the edge reports when it cannot perform an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeError {
    /// The edge is not configured / not reachable: public routes fail safely.
    Unavailable,
    /// The channel / bridge does not exist (anymore).
    NotFound,
    /// The edge refused the operation (bad state / argument).
    Refused,
    /// The request was sent but no answer arrived: outcome unknown.
    Timeout,
}

pub type EdgeResult<T> = Result<T, EdgeError>;

/// A SIP leg origination through a configured trunk.
#[derive(Debug, Clone)]
pub struct EdgeOriginate {
    /// The edge's trunk / endpoint configuration name for the route (from the route's
    /// SIP peer configuration reference; never a credential).
    pub trunk: String,
    pub destination: ExecutionNumber,
    pub caller_identity: ExecutionNumber,
    pub correlation_key: String,
    pub ring_timeout_seconds: u32,
}

/// Inbound dialog details as the edge observed them.
#[derive(Debug, Clone)]
pub struct EdgeInbound {
    /// Which configured peer / trunk the INVITE arrived on (validated peer only).
    pub peer: Option<String>,
    pub called: Option<ExecutionNumber>,
    pub caller: Option<ExecutionNumber>,
    /// A media join code presented by the application media side (internal peer).
    pub media_join_code: Option<String>,
}

/// Part F: an internal dialog of an enterprise device (desk phone / softphone).
#[derive(Debug, Clone)]
pub struct EdgeEnterpriseDialog {
    /// The edge endpoint the device authenticated as (validated; never a person's URI).
    pub endpoint: String,
    /// The dialled extension (an internal short code; never a telephone number).
    pub extension: String,
}

/// Normalized edge events.
#[derive(Clone)]
pub enum EdgeEventKind {
    /// A new inbound dialog entered Telephony's control application.
    Inbound(EdgeInbound),
    /// Part F: a new internal dialog of an enterprise device.
    EnterpriseInbound(EdgeEnterpriseDialog),
    /// An originated channel entered the control application (it answered).
    OriginatedAnswered,
    Ringing,
    Answered,
    Hangup {
        reason: EndReason,
        cause: u16,
    },
    Dtmf {
        digit: char,
    },
    Held,
    Unheld,
    EnteredBridge {
        bridge: String,
    },
    LeftBridge {
        bridge: String,
    },
    /// Part F: a spool recording (capture) of the channel finished — also when the caller
    /// hung up during it (the partial recording is kept).
    CaptureFinished {
        name: String,
    },
    /// Part F: a spool recording failed at the edge.
    CaptureFailed {
        name: String,
    },
}

impl fmt::Debug for EdgeEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdgeEventKind::Inbound(_) => f.write_str("Inbound(..)"),
            EdgeEventKind::EnterpriseInbound(_) => f.write_str("EnterpriseInbound(..)"),
            EdgeEventKind::OriginatedAnswered => f.write_str("OriginatedAnswered"),
            EdgeEventKind::Ringing => f.write_str("Ringing"),
            EdgeEventKind::Answered => f.write_str("Answered"),
            EdgeEventKind::Hangup { reason, cause } => write!(f, "Hangup({reason}, {cause})"),
            EdgeEventKind::Dtmf { .. } => f.write_str("Dtmf(<redacted>)"),
            EdgeEventKind::Held => f.write_str("Held"),
            EdgeEventKind::Unheld => f.write_str("Unheld"),
            EdgeEventKind::EnteredBridge { .. } => f.write_str("EnteredBridge"),
            EdgeEventKind::LeftBridge { .. } => f.write_str("LeftBridge"),
            EdgeEventKind::CaptureFinished { .. } => f.write_str("CaptureFinished"),
            EdgeEventKind::CaptureFailed { .. } => f.write_str("CaptureFailed"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EdgeEvent {
    /// Stable, unique event id (dedupe key at the edge provider).
    pub event_id: String,
    pub channel: ChannelRef,
    /// The channel is an application-media attachment (internal media peer), not a
    /// public-network call.
    pub application_media: bool,
    pub correlation_key: Option<String>,
    pub kind: EdgeEventKind,
    pub at: DateTime<Utc>,
}

/// A channel as the edge sees it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelState {
    Active(LegStatus),
    Gone,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub channel: ChannelRef,
    pub correlation_key: Option<String>,
    pub status: LegStatus,
}

/// The neutral SIP / media edge contract.
#[async_trait]
pub trait SipMediaEdge: Send + Sync {
    fn name(&self) -> &'static str;

    async fn health(&self) -> Health;

    /// Originates a SIP leg; returns the edge channel reference once the edge accepted
    /// the request. `Timeout` means the request may have been executed.
    async fn originate(&self, request: &EdgeOriginate) -> EdgeResult<ChannelRef>;

    /// Signals ringing to an inbound caller (no answer yet: no charge starts).
    async fn ring(&self, channel: &ChannelRef) -> EdgeResult<()>;

    /// Answers an inbound dialog.
    async fn answer(&self, channel: &ChannelRef) -> EdgeResult<()>;

    /// Hangs a channel up (terminate a leg).
    async fn hangup(&self, channel: &ChannelRef) -> EdgeResult<()>;

    /// Rejects an unanswered inbound dialog (SIP 403 class; reveals nothing).
    async fn reject(&self, channel: &ChannelRef) -> EdgeResult<()> {
        self.hangup(channel).await
    }

    async fn create_bridge(&self, name: &str) -> EdgeResult<String>;

    async fn add_to_bridge(&self, bridge: &str, channel: &ChannelRef) -> EdgeResult<()>;

    async fn remove_from_bridge(&self, bridge: &str, channel: &ChannelRef) -> EdgeResult<()>;

    async fn destroy_bridge(&self, bridge: &str) -> EdgeResult<()>;

    async fn hold(&self, channel: &ChannelRef) -> EdgeResult<()>;

    async fn resume(&self, channel: &ChannelRef) -> EdgeResult<()>;

    async fn send_dtmf(&self, channel: &ChannelRef, digits: &str) -> EdgeResult<()>;

    /// Transfer primitive: moves a channel to another edge endpoint. Business transfer
    /// permission is decided by Calling, never by the edge or a provider.
    async fn redirect(&self, channel: &ChannelRef, endpoint: &str) -> EdgeResult<()>;

    /// Part F: plays one edge-local sound (a built-in prompt mapped by Telephony) on a
    /// channel. Playbacks of one channel queue in order.
    /// Part F: records the channel's caller audio (WAV) into the edge's TRANSIENT spool
    /// under `name` (Telephony's own name, never a caller-supplied one), at most
    /// `max_seconds`, optionally after a beep; `#` ends it early. Completion arrives as
    /// [`EdgeEventKind::CaptureFinished`] / [`EdgeEventKind::CaptureFailed`].
    async fn record(
        &self,
        _channel: &ChannelRef,
        _name: &str,
        _max_seconds: u32,
        _beep: bool,
    ) -> EdgeResult<()> {
        Err(EdgeError::Refused)
    }

    /// Part F: records a bridge's MIXED audio (a call recording of a bridged leg) into the
    /// spool under `name`; completion arrives as for [`SipMediaEdge::record`].
    async fn record_bridge(
        &self,
        _bridge: &str,
        _name: &str,
        _max_seconds: u32,
        _beep: bool,
    ) -> EdgeResult<()> {
        Err(EdgeError::Refused)
    }

    /// Part F: stops a running spool recording early (its end event follows). A
    /// recording that is not running is [`EdgeError::NotFound`].
    async fn stop_recording(&self, _name: &str) -> EdgeResult<()> {
        Err(EdgeError::NotFound)
    }

    /// Part F: the bytes of a finished spool recording (at most `max_bytes`).
    async fn fetch_recording(&self, _name: &str, _max_bytes: usize) -> EdgeResult<Vec<u8>> {
        Err(EdgeError::Unavailable)
    }

    /// Part F: removes a spool recording. Idempotent: a missing recording is success.
    async fn delete_recording(&self, _name: &str) -> EdgeResult<()> {
        Ok(())
    }

    async fn play(&self, _channel: &ChannelRef, _sound: &str) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }

    async fn query(&self, channel: &ChannelRef) -> ChannelState;

    /// Finds the channel created for a correlation key (status read-back after an
    /// unknown outcome).
    async fn find_by_correlation(
        &self,
        correlation_key: &str,
    ) -> EdgeResult<Option<ChannelSummary>>;

    /// All live channels (reconciliation).
    async fn list(&self) -> EdgeResult<Vec<ChannelSummary>>;

    /// Since when this process has observed EVERY channel end on the edge (its event
    /// stream continuously connected). `find_by_correlation` returning `None` proves "never
    /// created" only for requests transmitted after this instant; `None` = no such proof.
    fn history_complete_since(&self) -> Option<DateTime<Utc>> {
        None
    }

    /// Media evidence of a channel: (RTP packets received, RTP packets sent).
    async fn media_stats(&self, _channel: &ChannelRef) -> Option<(u64, u64)> {
        None
    }

    /// The edge event stream.
    fn subscribe(&self) -> broadcast::Receiver<EdgeEvent>;
}

/// No SIP / media edge configured: every SIP operation fails safely (`Unavailable`);
/// application RTC is unaffected (Calling serves it without Telephony).
pub struct NoEdge {
    events: broadcast::Sender<EdgeEvent>,
}

impl Default for NoEdge {
    fn default() -> Self {
        Self {
            events: broadcast::channel(1).0,
        }
    }
}

impl NoEdge {
    pub fn shared() -> Arc<dyn SipMediaEdge> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl SipMediaEdge for NoEdge {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn health(&self) -> Health {
        Health::Unavailable
    }
    async fn originate(&self, _: &EdgeOriginate) -> EdgeResult<ChannelRef> {
        Err(EdgeError::Unavailable)
    }
    async fn ring(&self, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn answer(&self, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn hangup(&self, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn create_bridge(&self, _: &str) -> EdgeResult<String> {
        Err(EdgeError::Unavailable)
    }
    async fn add_to_bridge(&self, _: &str, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn remove_from_bridge(&self, _: &str, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn destroy_bridge(&self, _: &str) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn hold(&self, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn resume(&self, _: &ChannelRef) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn send_dtmf(&self, _: &ChannelRef, _: &str) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn redirect(&self, _: &ChannelRef, _: &str) -> EdgeResult<()> {
        Err(EdgeError::Unavailable)
    }
    async fn query(&self, _: &ChannelRef) -> ChannelState {
        ChannelState::Unknown
    }
    async fn find_by_correlation(&self, _: &str) -> EdgeResult<Option<ChannelSummary>> {
        Err(EdgeError::Unavailable)
    }
    async fn list(&self) -> EdgeResult<Vec<ChannelSummary>> {
        Err(EdgeError::Unavailable)
    }
    fn subscribe(&self) -> broadcast::Receiver<EdgeEvent> {
        self.events.subscribe()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EdgeConfigError {
    #[error("{0} is required for the selected SIP / media edge")]
    Missing(&'static str),
    #[error("{0} is invalid: {1}")]
    Invalid(&'static str, String),
}

/// Selects the SIP / media edge from the environment:
/// `TELEPHONY_SIP_EDGE=none` (default) or `asterisk-ari` (see [`ari::AriSettings`]).
pub fn edge_from_env(
    environment: cf_service_auth::RuntimeEnvironment,
) -> Result<Arc<dyn SipMediaEdge>, EdgeConfigError> {
    match std::env::var("TELEPHONY_SIP_EDGE")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        None | Some("") | Some("none") => Ok(NoEdge::shared()),
        Some("asterisk-ari") => {
            let settings = ari::AriSettings::from_env(environment)?;
            Ok(Arc::new(ari::AriEdge::start(settings)))
        }
        Some(_) => Err(EdgeConfigError::Invalid(
            "TELEPHONY_SIP_EDGE",
            "must be none or asterisk-ari".into(),
        )),
    }
}
