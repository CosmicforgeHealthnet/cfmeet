//! SIP-trunk interconnect adapter (Part E §42–§43).
//!
//! The first interconnect: a lawful carrier / aggregator reached through a SIP trunk on
//! CosmicForge's own SIP / media edge (media anchored at the edge). Origination, control,
//! status read-back and events go through [`SipMediaEdge`]; the trunk's credentials and
//! peer addresses live in the edge's secret-injected configuration, never in Telephony's
//! database or this code. The provider call reference IS the edge channel reference.

use std::sync::Arc;

use async_trait::async_trait;

use super::{
    CapacityReport, ControlOutcome, EventRejection, OriginateOutcome, OriginateRequest,
    ProviderEvent, ProviderIngress, QueryKey, QueryOutcome, TelecomInterconnectProvider,
    UnknownReason,
};
use crate::{
    domain::{EndReason, FailureCategory, Health, LegStatus, ProviderCapability},
    sip::{ChannelState, EdgeError, EdgeOriginate, SipMediaEdge},
};

pub struct SipTrunkProvider {
    id: String,
    capabilities: Vec<ProviderCapability>,
    edge: Arc<dyn SipMediaEdge>,
}

impl SipTrunkProvider {
    pub fn new(
        id: &str,
        capabilities: Vec<ProviderCapability>,
        edge: Arc<dyn SipMediaEdge>,
    ) -> Self {
        Self {
            id: id.to_string(),
            capabilities,
            edge,
        }
    }

    pub fn edge(&self) -> &Arc<dyn SipMediaEdge> {
        &self.edge
    }
}

#[async_trait]
impl TelecomInterconnectProvider for SipTrunkProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &[ProviderCapability] {
        &self.capabilities
    }

    async fn originate(&self, request: OriginateRequest) -> OriginateOutcome {
        let Some(trunk) = request.sip_peer_config_ref.clone() else {
            return OriginateOutcome::DefiniteFailure {
                category: FailureCategory::Routing,
                reason: EndReason::Failed,
                transmitted: false,
            };
        };
        let edge_request = EdgeOriginate {
            trunk,
            destination: request.destination,
            caller_identity: request.caller_identity,
            correlation_key: request.correlation_key,
            ring_timeout_seconds: request.ring_timeout.as_secs().clamp(5, 300) as u32,
        };
        match self.edge.originate(&edge_request).await {
            Ok(channel) => OriginateOutcome::Accepted {
                provider_call_ref: channel,
                provider_idempotency_ref: None,
            },
            // The edge refused before creating a channel: nothing was dialled.
            Err(EdgeError::Refused) | Err(EdgeError::NotFound) => {
                OriginateOutcome::DefiniteFailure {
                    category: FailureCategory::Routing,
                    reason: EndReason::Failed,
                    transmitted: true,
                }
            }
            // The edge could not be reached at all (connection refused before any byte):
            // definitely not dialled.
            Err(EdgeError::Unavailable) => OriginateOutcome::DefiniteFailure {
                category: FailureCategory::Network,
                reason: EndReason::Failed,
                transmitted: false,
            },
            // Sent, no answer: the edge may have created the channel.
            Err(EdgeError::Timeout) => OriginateOutcome::UnknownOutcome {
                reason: UnknownReason::TimeoutAfterSend,
            },
        }
    }

    async fn terminate(&self, provider_call_ref: &str) -> ControlOutcome {
        match self.edge.hangup(&provider_call_ref.to_string()).await {
            Ok(()) => ControlOutcome::Done,
            Err(EdgeError::NotFound) => ControlOutcome::AlreadyGone,
            Err(EdgeError::Timeout) | Err(EdgeError::Unavailable) => ControlOutcome::Unknown,
            Err(EdgeError::Refused) => ControlOutcome::DefiniteFailure,
        }
    }

    async fn hold(&self, provider_call_ref: &str) -> ControlOutcome {
        match self.edge.hold(&provider_call_ref.to_string()).await {
            Ok(()) => ControlOutcome::Done,
            Err(EdgeError::NotFound) => ControlOutcome::AlreadyGone,
            Err(EdgeError::Timeout) | Err(EdgeError::Unavailable) => ControlOutcome::Unknown,
            Err(EdgeError::Refused) => ControlOutcome::DefiniteFailure,
        }
    }

    async fn resume(&self, provider_call_ref: &str) -> ControlOutcome {
        match self.edge.resume(&provider_call_ref.to_string()).await {
            Ok(()) => ControlOutcome::Done,
            Err(EdgeError::NotFound) => ControlOutcome::AlreadyGone,
            Err(EdgeError::Timeout) | Err(EdgeError::Unavailable) => ControlOutcome::Unknown,
            Err(EdgeError::Refused) => ControlOutcome::DefiniteFailure,
        }
    }

    async fn query_call(&self, key: &QueryKey) -> QueryOutcome {
        if let Some(channel) = &key.provider_call_ref {
            return match self.edge.query(channel).await {
                ChannelState::Active(status) => QueryOutcome::Active {
                    provider_call_ref: channel.clone(),
                    status,
                },
                // The edge keeps no history: a vanished channel ended.
                ChannelState::Gone => QueryOutcome::Ended {
                    provider_call_ref: channel.clone(),
                    reason: EndReason::Completed,
                },
                ChannelState::Unknown => QueryOutcome::Unknown,
            };
        }
        match self.edge.find_by_correlation(&key.correlation_key).await {
            // A channel the edge saw end (its end history).
            Ok(Some(summary)) if summary.status.is_terminal() => QueryOutcome::Ended {
                provider_call_ref: summary.channel,
                reason: if summary.status == LegStatus::Ended {
                    EndReason::Completed
                } else {
                    EndReason::Failed
                },
            },
            Ok(Some(summary)) => QueryOutcome::Active {
                provider_call_ref: summary.channel,
                status: summary.status,
            },
            // No live channel and no recorded end for this key. That proves "never
            // created" only if the edge's end history covers the transmission instant
            // (event stream connected throughout); otherwise a channel may have existed
            // and ended unobserved (e.g. across a restart), so the answer is Unknown —
            // never Absent, which would permit a second origination.
            Ok(None) => match (self.edge.history_complete_since(), key.transmitted_at) {
                (Some(since), Some(sent)) if since < sent => QueryOutcome::Absent,
                _ => QueryOutcome::Unknown,
            },
            Err(_) => QueryOutcome::Unknown,
        }
    }

    async fn list_active(&self) -> Option<Vec<super::ActiveCall>> {
        let channels = self.edge.list().await.ok()?;
        Some(
            channels
                .into_iter()
                .filter(|c| !c.status.is_terminal())
                .map(|c| super::ActiveCall {
                    provider_call_ref: c.channel,
                    correlation_key: c.correlation_key,
                    status: c.status,
                })
                .collect(),
        )
    }

    async fn health(&self) -> Health {
        self.edge.health().await
    }

    async fn capacity(&self) -> CapacityReport {
        CapacityReport {
            max_concurrent: None,
            in_use: self.edge.list().await.ok().map(|l| l.len() as u32),
        }
    }

    /// SIP-trunk events arrive over the authenticated edge control plane, never as
    /// public HTTP callbacks: every HTTP callback for this adapter is refused.
    fn verify_event(&self, _: &ProviderIngress) -> Result<(), EventRejection> {
        Err(EventRejection::Unauthenticated)
    }

    fn normalize_event(&self, _: &ProviderIngress) -> Result<Vec<ProviderEvent>, EventRejection> {
        Err(EventRejection::Malformed)
    }
}
