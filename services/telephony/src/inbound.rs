//! Phone-to-app / phone-to-phone inbound ingress (Part E §45, §84–§92, §176).
//!
//! An inbound public call terminates at the SIP / media edge, inside Telephony — never at
//! Calling's public API. Before any business state exists:
//!
//! 1. kill switches (Telephony, public inbound);
//! 2. peer validation: only a configured, enabled inbound interconnect (its SIP peer
//!    configuration) may deliver calls, and only to a platform relay number bound to it;
//!    anything else is rejected and raises SIP_PEER_REJECTED;
//! 3. bounded ingress rate per peer;
//! 4. the caller hears ringing (no answer: nothing is charged yet);
//! 5. Privacy Relay resolves the called relay number to its CURRENT mapping (generation,
//!    assignment, session, context) — expired, cooled-down, unknown or ambiguous mappings
//!    are rejected with no information about any previous destination;
//! 6. the presented caller number goes straight to the Contact Vault; only its
//!    ContactReference continues (anonymous callers are rejected);
//! 7. Calling receives an authenticated offer and creates the ONE canonical Call.
//!
//! The leg is then RINGING until Calling bridges it (an application endpoint accepted, or
//! the second public leg answered) or terminates it.

use sea_orm::{FromQueryResult, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    domain::{LegStatus, NormalizedEvent, PublicRouteClass},
    error::TelephonyResult,
    incidents::{self, Incident, Severity},
    legs,
    providers::ExecutionNumber,
    service::TelephonyService,
    sip::{EdgeEvent, EdgeInbound},
};

/// Inbound dialogs accepted per peer per minute before further ones are rejected.
pub const MAX_INBOUND_PER_PEER_PER_MINUTE: i64 = 600;

#[derive(Debug, FromQueryResult)]
struct InboundRoute {
    route_id: String,
    provider_id: String,
    max_call_seconds: i32,
}

/// Why an inbound dialog was rejected (bounded codes; logs only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundRejection {
    Disabled,
    UnknownPeer,
    UnboundNumber,
    RateLimited,
    NoActiveMapping,
    AnonymousCaller,
    CallingUnavailable,
    Internal,
}

impl InboundRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            InboundRejection::Disabled => "DISABLED",
            InboundRejection::UnknownPeer => "UNKNOWN_PEER",
            InboundRejection::UnboundNumber => "UNBOUND_NUMBER",
            InboundRejection::RateLimited => "RATE_LIMITED",
            InboundRejection::NoActiveMapping => "NO_ACTIVE_MAPPING",
            InboundRejection::AnonymousCaller => "ANONYMOUS_CALLER",
            InboundRejection::CallingUnavailable => "CALLING_UNAVAILABLE",
            InboundRejection::Internal => "INTERNAL",
        }
    }
}

fn opaque_reference(channel: &str) -> String {
    format!(
        "edge-{}",
        &hex::encode(Sha256::digest(channel.as_bytes()))[..24]
    )
}

impl TelephonyService {
    pub(crate) async fn handle_inbound_dialog(
        &self,
        event: &EdgeEvent,
        inbound: EdgeInbound,
    ) -> TelephonyResult<()> {
        match self.accept_inbound(event, &inbound).await {
            Ok(leg_id) => {
                tracing::info!(telephony_leg_id = %leg_id, "inbound public call offered to Calling");
                Ok(())
            }
            Err(rejection) => {
                tracing::info!(reason = rejection.as_str(), "inbound public call rejected");
                let _ = self.edge.reject(&event.channel).await;
                Ok(())
            }
        }
    }

    async fn accept_inbound(
        &self,
        event: &EdgeEvent,
        inbound: &EdgeInbound,
    ) -> Result<Uuid, InboundRejection> {
        let internal = |_| InboundRejection::Internal;
        if !self.flags.telephony.enabled() || !self.flags.public_inbound.enabled() {
            return Err(InboundRejection::Disabled);
        }
        let (Some(peer), Some(called)) = (inbound.peer.clone(), inbound.called.clone()) else {
            return Err(InboundRejection::UnknownPeer);
        };
        let route: Option<InboundRoute> = db::one(
            &self.db,
            "SELECT r.id AS route_id, r.provider_id, r.max_call_seconds FROM telecom_routes r \
             JOIN telecom_providers p ON p.id = r.provider_id \
             JOIN telecom_number_bindings b ON b.route_id = r.id \
             WHERE r.sip_peer_config_ref_id = $1 AND r.status = 'ENABLED' AND p.status = 'ENABLED' \
               AND 'VOICE_INBOUND' = ANY(r.capabilities) AND b.public_e164 = $2 AND b.status = 'ENABLED' \
               AND 'VOICE_INBOUND' = ANY(b.capabilities) LIMIT 1",
            vec![peer.clone().into(), called.expose().into()],
        )
        .await
        .map_err(internal)?;
        let Some(route) = route else {
            let known_peer = db::scalar_i64(
                &self.db,
                "SELECT count(*) FROM sip_peer_config_refs WHERE id = $1",
                vec![peer.clone().into()],
            )
            .await
            .map_err(internal)?;
            let now = self.now();
            let mut incident = Incident::new(
                "SIP_PEER_REJECTED",
                Severity::Medium,
                format!(
                    "sip-peer:{}",
                    if known_peer > 0 {
                        peer.as_str()
                    } else {
                        "unknown"
                    }
                ),
            )
            .evidence(json!({ "knownPeer": known_peer > 0 }));
            incident.route_id = None;
            incidents::raise(&self.db, &incident, now)
                .await
                .map_err(internal)?;
            return Err(if known_peer > 0 {
                InboundRejection::UnboundNumber
            } else {
                InboundRejection::UnknownPeer
            });
        };
        let hits = self.count_ingress(&peer).await.map_err(internal)?;
        if hits > MAX_INBOUND_PER_PEER_PER_MINUTE {
            return Err(InboundRejection::RateLimited);
        }
        // The caller hears ringing; nothing is answered (charged) yet.
        let _ = self.edge.ring(&event.channel).await;
        let correlation = cf_service_auth::CorrelationId::generate();
        let mapping = self
            .privacy
            .resolve_inbound(
                &called,
                Some(&opaque_reference(&event.channel)),
                event.at,
                Some(event.at),
                correlation.as_str(),
            )
            .await
            .map_err(|_| InboundRejection::NoActiveMapping)?;
        let caller = inbound
            .caller
            .clone()
            .ok_or(InboundRejection::AnonymousCaller)?;
        let protected = self
            .privacy
            .protect_inbound_caller(mapping.decision_id, &caller, correlation.as_str())
            .await
            .map_err(|_| InboundRejection::NoActiveMapping)?;
        drop(caller);
        let leg_id = Uuid::new_v4();
        let offer = json!({
            "schemaVersion": "1",
            "telephonyLegId": leg_id,
            "correlationId": correlation.as_str(),
            "scope": {
                "tenantId": mapping.tenant_id,
                "applicationId": mapping.application_id,
                "environmentId": mapping.environment_id,
            },
            "inboundResolution": {
                "decisionId": mapping.decision_id,
                "contextId": mapping.context_id,
                "contextVersion": mapping.context_version,
                "relaySessionId": mapping.relay_session_id,
                "relaySessionVersion": mapping.relay_session_version,
                "assignmentId": mapping.assignment_id,
                "assignmentGeneration": mapping.assignment_generation,
                "validUntil": mapping.valid_until,
            },
            "callerContactReference": protected.contact_reference,
            "allowedTargets": protected.allowed_targets,
            "maxDurationSeconds": route.max_call_seconds,
        });
        let accepted: Value = self
            .calling
            .offer_inbound(&offer)
            .await
            .map_err(|_| InboundRejection::CallingUnavailable)?;
        let parse = |k: &str| accepted[k].as_str().and_then(|s| Uuid::parse_str(s).ok());
        let (Some(call_id), Some(call_leg_id)) = (parse("callId"), parse("callLegId")) else {
            return Err(InboundRejection::CallingUnavailable);
        };
        let class = accepted["routeClass"]
            .as_str()
            .and_then(PublicRouteClass::parse)
            .filter(|c| matches!(c, PublicRouteClass::PstnToApp | PublicRouteClass::PstnRelay))
            .ok_or(InboundRejection::CallingUnavailable)?;
        let now = self.now();
        let persisted: TelephonyResult<()> = async {
            let txn = self.db.begin().await?;
            db::exec(
                &txn,
                "INSERT INTO telephony_legs (id, calling_call_id, calling_call_leg_id, tenant_id, application_id, \
                   environment_id, direction, route_class, route_id, provider_id, status, generation, contact_reference, \
                   relay_session_id, context_id, inbound_resolution_id, max_duration_seconds, media_attachment, \
                   usage_status, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'INBOUND', $7, $8, $9, 'PLANNED', 1, $10, $11, $12, $13, $14, $15, \
                   'PENDING', $16, $16)",
                vec![
                    leg_id.into(),
                    call_id.into(),
                    call_leg_id.into(),
                    mapping.tenant_id.into(),
                    mapping.application_id.into(),
                    mapping.environment_id.into(),
                    class.as_str().into(),
                    route.route_id.clone().into(),
                    route.provider_id.clone().into(),
                    protected.contact_reference.into(),
                    mapping.relay_session_id.into(),
                    mapping.context_id.into(),
                    mapping.decision_id.into(),
                    route.max_call_seconds.clamp(30, 86_400).into(),
                    if class == PublicRouteClass::PstnToApp { "APPLICATION" } else { "NONE" }.into(),
                    now.into(),
                ],
            )
            .await?;
            db::exec(
                &txn,
                "INSERT INTO provider_call_bindings (id, telephony_leg_id, provider_id, route_id, correlation_key, \
                   provider_call_ref, status, created_at, sent_at, bound_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'BOUND', $7, $7, $7, $7)",
                vec![
                    Uuid::new_v4().into(),
                    leg_id.into(),
                    route.provider_id.clone().into(),
                    route.route_id.clone().into(),
                    format!("cfm-{}", Uuid::new_v4().simple()).into(),
                    event.channel.clone().into(),
                    now.into(),
                ],
            )
            .await?;
            let leg = legs::lock(&txn, leg_id).await?;
            // PLANNED -> ORIGINATING (silent) -> RINGING: the inbound dialog is ringing.
            let leg = legs::transition_in(
                &txn,
                &leg,
                &legs::Change::to(LegStatus::Originating, NormalizedEvent::LegAccepted, now).silent(),
            )
            .await?
            .unwrap_or(leg);
            let leg = legs::transition_in(&txn, &leg, &legs::Change::to(LegStatus::Ringing, NormalizedEvent::LegRinging, now))
                .await?
                .unwrap_or(leg);
            drop(leg);
            txn.commit().await?;
            Ok(())
        }
        .await;
        if persisted.is_err() {
            return Err(InboundRejection::Internal);
        }
        Ok(leg_id)
    }

    async fn count_ingress(&self, peer: &str) -> TelephonyResult<i64> {
        let now = self.now();
        let window = chrono::DateTime::<chrono::Utc>::from_timestamp(
            now.timestamp() - now.timestamp() % 60,
            0,
        )
        .unwrap_or(now);
        let hits = db::scalar_i64(
            &self.db,
            "INSERT INTO telephony_ingress_windows (subject_kind, subject_id, window_start, hits) VALUES ('SIP_INGRESS', $1, $2, 1) \
             ON CONFLICT (subject_kind, subject_id, window_start) DO UPDATE SET hits = telephony_ingress_windows.hits + 1 \
             RETURNING hits",
            vec![peer.to_ascii_lowercase().into(), window.into()],
        )
        .await?;
        Ok(hits)
    }
}

/// Parses a caller / called identity from the edge (canonical E.164 only).
pub fn edge_number(raw: Option<&str>) -> Option<ExecutionNumber> {
    raw.and_then(ExecutionNumber::parse)
}
