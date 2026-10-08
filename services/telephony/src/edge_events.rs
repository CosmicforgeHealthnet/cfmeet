//! The edge event pump (Part E §45, §49, §53, §91).
//!
//! Consumes the SIP / media edge's event stream:
//!
//! * an application-media join (internal media peer presenting a media join code) is
//!   attached to its bridge — or hung up when the code is unknown, used or late;
//! * an inbound public-network dialog starts the inbound flow (§45);
//! * progress of a channel Telephony originated (or accepted) becomes a normalized
//!   provider event of the interconnect that owns the channel and goes through the same
//!   dedupe / ordering / quarantine pipeline as HTTP callbacks.

use std::collections::BTreeMap;

use sea_orm::FromQueryResult;
use tokio::sync::broadcast::error::RecvError;

use crate::{
    db,
    domain::NormalizedEvent,
    error::TelephonyResult,
    providers::ProviderEvent,
    service::TelephonyService,
    sip::{EdgeEvent, EdgeEventKind},
};

#[derive(Debug, FromQueryResult)]
struct Owner {
    provider_id: String,
}

impl TelephonyService {
    /// Runs the pump forever (one per instance; each instance has its own edge session).
    pub async fn run_edge_events(self) {
        let mut rx = self.edge.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(e) = self.handle_edge_event(event).await {
                        tracing::warn!(error = %e, "edge event handling failed");
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    // Missed events are recovered by reconciliation (read-back).
                    tracing::warn!(
                        missed = n,
                        "edge event stream lagged; reconciliation will catch up"
                    );
                }
                Err(RecvError::Closed) => return,
            }
        }
    }

    /// Handles one edge event (public for deterministic tests).
    pub async fn handle_edge_event(&self, event: EdgeEvent) -> TelephonyResult<()> {
        match &event.kind {
            EdgeEventKind::Inbound(inbound) => {
                if let Some(code) = &inbound.media_join_code {
                    if !self.application_media_joined(code, &event.channel).await? {
                        let _ = self.edge.hangup(&event.channel).await;
                    }
                    return Ok(());
                }
                self.handle_inbound_dialog(&event, inbound.clone()).await
            }
            kind => {
                let normalized = match kind {
                    EdgeEventKind::Ringing => Some(NormalizedEvent::LegRinging),
                    EdgeEventKind::Answered | EdgeEventKind::OriginatedAnswered => {
                        Some(NormalizedEvent::LegAnswered)
                    }
                    EdgeEventKind::Held => Some(NormalizedEvent::LegHeld),
                    EdgeEventKind::Unheld => Some(NormalizedEvent::LegResumed),
                    EdgeEventKind::Hangup { reason, .. } => Some(match reason {
                        crate::domain::EndReason::Busy => NormalizedEvent::LegBusy,
                        crate::domain::EndReason::NoAnswer => NormalizedEvent::LegNoAnswer,
                        crate::domain::EndReason::Declined => NormalizedEvent::LegDeclined,
                        crate::domain::EndReason::Completed => NormalizedEvent::LegEnded,
                        _ => NormalizedEvent::LegFailed,
                    }),
                    EdgeEventKind::Dtmf { .. } => Some(NormalizedEvent::LegDtmf),
                    EdgeEventKind::LeftBridge { .. } => {
                        self.bridge_member_left(None, Some(&event.channel)).await?;
                        None
                    }
                    _ => None,
                };
                if let EdgeEventKind::Hangup { .. } = kind {
                    // An application-media channel hanging up leaves its bridge.
                    self.bridge_member_left(None, Some(&event.channel)).await?;
                }
                let Some(normalized) = normalized else {
                    return Ok(());
                };
                // The interconnect that owns this channel (by binding).
                let owner: Option<Owner> = db::one(
                    &self.db,
                    "SELECT provider_id FROM provider_call_bindings WHERE provider_call_ref = $1 \
                     OR correlation_key = $2 ORDER BY created_at DESC LIMIT 1",
                    vec![
                        event.channel.clone().into(),
                        event.correlation_key.clone().unwrap_or_default().into(),
                    ],
                )
                .await?;
                let provider_id = match owner {
                    Some(o) => o.provider_id,
                    // Not ours: quarantine under the edge's own provider identity.
                    None => match self.providers.ids().next() {
                        Some(id) => id.clone(),
                        None => return Ok(()),
                    },
                };
                let mut detail = BTreeMap::new();
                if let EdgeEventKind::Hangup { cause, .. } = kind {
                    detail.insert("hangup_cause".to_string(), cause.to_string());
                }
                self.ingest_provider_event(
                    &provider_id,
                    ProviderEvent {
                        provider_event_id: event.event_id.clone(),
                        provider_call_ref: Some(event.channel.clone()),
                        correlation_key: event.correlation_key.clone(),
                        event: normalized,
                        occurred_at: event.at,
                        sequence: None,
                        restricted_detail: detail,
                        inbound: None,
                        dtmf: None,
                        provider_cost_minor: None,
                    },
                )
                .await?;
                Ok(())
            }
        }
    }
}
