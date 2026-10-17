//! Phone-to-app / phone-to-phone inbound ingress (Part E §45, §84–§92, §176).
//!
//! An inbound public call terminates at the SIP / media edge, inside Telephony — never at
//! Calling's public API. Before any business state exists:
//!
//! 1. kill switches (Telephony, public inbound);
//! 2. peer validation: only a configured, enabled inbound interconnect (its SIP peer
//!    configuration) may deliver calls, and only to a platform relay number bound to it;
//!    anything else is rejected and raises SIP_PEER_REJECTED. When the edge reports the
//!    dialog's source address it must fall inside the peer's `allowed_source_cidrs`
//!    (the edge's own `identify` match is the first line; this is the second), and the
//!    peer's `max_concurrent_dialogs` is enforced;
//! 3. bounded ingress rate per peer (over the bound: `SIP_INGRESS_RATE_LIMITED`, one
//!    deduplicated incident per peer, a bounded metric);
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
    domain::{InterconnectClass, LegStatus, NormalizedEvent, PublicRouteClass},
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
    /// `RELAY` (Privacy Relay relay number) or `DCN` (Numbering-owned number).
    number_kind: String,
    number_resource_id: Option<Uuid>,
    /// Part IV L: `PUBLIC` or `SYNTHETIC_TEST`.
    interconnect_class: String,
}

impl InboundRoute {
    fn interconnect(&self) -> InterconnectClass {
        InterconnectClass::parse(&self.interconnect_class).unwrap_or(InterconnectClass::Public)
    }
}

#[derive(Debug, FromQueryResult)]
struct PeerRow {
    cidrs: Vec<String>,
    max_concurrent_dialogs: i32,
}

/// Whether `ip` lies inside `cidr` (`a.b.c.d/n`, `x::/n`, or a bare address).
pub fn cidr_contains(cidr: &str, ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    let (net, bits) = match cidr.split_once('/') {
        Some((n, b)) => (n.trim(), b.trim().parse::<u32>().ok()),
        None => (cidr.trim(), None),
    };
    let Ok(net) = net.parse::<IpAddr>() else {
        return false;
    };
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(a)) => {
            let bits = bits.unwrap_or(32);
            if bits > 32 {
                return false;
            }
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            u32::from(n) & mask == u32::from(a) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(a)) => {
            let bits = bits.unwrap_or(128);
            if bits > 128 {
                return false;
            }
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            u128::from(n) & mask == u128::from(a) & mask
        }
        _ => false,
    }
}

/// Why an inbound dialog was rejected (bounded codes; logs only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundRejection {
    Disabled,
    UnknownPeer,
    UnboundNumber,
    /// The reported source address is outside the peer's allowed networks.
    SourceNotAllowed,
    /// The peer's `max_concurrent_dialogs` is reached.
    PeerCapacity,
    RateLimited,
    NoActiveMapping,
    /// Privacy Relay could not be reached (fails closed; incident).
    PrivacyRelayUnavailable,
    /// Part IV L: Numbering could not be reached for a DCN (fails closed; incident).
    NumberingUnavailable,
    /// Part IV L: the DCN is not routable now (unassigned, suspended, releasing,
    /// quarantined, capability or route not ready): never a former owner.
    DcnNotRoutable,
    /// Part IV L: the DCN is assigned but its voice routes nowhere.
    DcnNotRouted,
    /// Part IV L: Numbering's answer does not match the local DCN binding.
    DcnMismatch,
    AnonymousCaller,
    /// Calling could not be reached (transport failure, timeout, 5xx): fails closed.
    CallingUnavailable,
    /// Calling answered and explicitly refused the route (e.g. PUBLIC_ROUTE_DENIED, the
    /// target unavailable, a spend limit).
    CallingRefused,
    /// Calling answered with an authorization / policy denial (CALL_NOT_AUTHORIZED, a
    /// grant it could not verify, a live-telecom or internal-permission refusal).
    CallingDenied,
    /// Calling answered 2xx with a body that is not a valid acceptance.
    CallingInvalidResponse,
    /// Part IV L: the call arrived on a synthetic test route that is not enabled.
    InterconnectDisabled,
    /// Part IV L: a synthetic number on a public route, a real one (or a relay number) on
    /// a synthetic test route.
    InterconnectMismatch,
    Internal,
}

impl InboundRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            InboundRejection::Disabled => "DISABLED",
            InboundRejection::UnknownPeer => "UNKNOWN_PEER",
            InboundRejection::UnboundNumber => "UNBOUND_NUMBER",
            InboundRejection::SourceNotAllowed => "SOURCE_NOT_ALLOWED",
            InboundRejection::PeerCapacity => "PEER_CAPACITY",
            InboundRejection::RateLimited => "RATE_LIMITED",
            InboundRejection::NoActiveMapping => "NO_ACTIVE_MAPPING",
            InboundRejection::PrivacyRelayUnavailable => "PRIVACY_RELAY_UNAVAILABLE",
            InboundRejection::NumberingUnavailable => "NUMBERING_UNAVAILABLE",
            InboundRejection::DcnNotRoutable => "DCN_NOT_ROUTABLE",
            InboundRejection::DcnNotRouted => "DCN_NOT_ROUTED",
            InboundRejection::DcnMismatch => "DCN_MISMATCH",
            InboundRejection::AnonymousCaller => "ANONYMOUS_CALLER",
            InboundRejection::CallingUnavailable => "CALLING_UNAVAILABLE",
            InboundRejection::CallingRefused => "CALLING_REFUSED",
            InboundRejection::CallingDenied => "CALLING_DENIED",
            InboundRejection::CallingInvalidResponse => "CALLING_INVALID_RESPONSE",
            InboundRejection::InterconnectDisabled => "INTERCONNECT_DISABLED",
            InboundRejection::InterconnectMismatch => "INTERCONNECT_MISMATCH",
            InboundRejection::Internal => "INTERNAL",
        }
    }
}

/// A Privacy Relay failure during inbound resolution: unreachable is its own (incident)
/// rejection; any answer refusing the mapping reveals nothing beyond "no mapping".
fn relay_rejection(error: crate::ports::PortError) -> InboundRejection {
    match error {
        crate::ports::PortError::Unavailable => InboundRejection::PrivacyRelayUnavailable,
        _ => InboundRejection::NoActiveMapping,
    }
}

/// Calling's answer to an inbound offer that is not an acceptance (Step 4 of the L
/// closure): an unreachable Calling, an explicit route refusal and an authorization /
/// policy denial are distinct outcomes. Every one rejects the dialog (fail closed).
pub fn calling_rejection(error: crate::ports::PortError) -> InboundRejection {
    use crate::ports::PortError;
    match error {
        PortError::Unavailable => InboundRejection::CallingUnavailable,
        PortError::NotFound => InboundRejection::CallingRefused,
        PortError::Denied(code) => match code.as_str() {
            "CALL_NOT_AUTHORIZED"
            | "INTERNAL_AUTHENTICATION_FAILED"
            | "INTERNAL_PERMISSION_DENIED"
            | "AUTHORIZATION_REQUIRED" => InboundRejection::CallingDenied,
            _ => InboundRejection::CallingRefused,
        },
    }
}

/// The Call and leg of Calling's acceptance; anything else is an invalid response.
fn accepted_refs(accepted: &Value) -> Result<(Uuid, Uuid), InboundRejection> {
    let parse = |k: &str| accepted[k].as_str().and_then(|s| Uuid::parse_str(s).ok());
    match (parse("callId"), parse("callLegId")) {
        (Some(call), Some(leg)) => Ok((call, leg)),
        _ => Err(InboundRejection::CallingInvalidResponse),
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
                crate::metrics::inc("telephony_inbound_offers_total", &[("result", "OFFERED")]);
                tracing::info!(telephony_leg_id = %leg_id, "inbound public call offered to Calling");
                Ok(())
            }
            Err(rejection) => {
                crate::metrics::inc(
                    "telephony_inbound_offers_total",
                    &[("result", rejection.as_str())],
                );
                tracing::info!(reason = rejection.as_str(), "inbound public call rejected");
                let _ = self.edge.reject(&event.channel).await;
                match rejection {
                    InboundRejection::PrivacyRelayUnavailable => {
                        self.privacy_relay_unavailable("RESOLVE_INBOUND").await?
                    }
                    // Privacy Relay answered (no active mapping): it is reachable.
                    InboundRejection::NoActiveMapping => self.privacy_relay_available().await?,
                    InboundRejection::NumberingUnavailable => self.numbering_unavailable().await?,
                    _ => {}
                }
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
            "SELECT r.id AS route_id, r.provider_id, r.max_call_seconds, b.number_kind, b.number_resource_id, \
               r.interconnect_class \
             FROM telecom_routes r \
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
        // The peer's declared networks and dialog bound (`sip_peer_config_refs`).
        let peer_row: Option<PeerRow> = db::one(
            &self.db,
            "SELECT allowed_source_cidrs::text[] AS cidrs, max_concurrent_dialogs \
             FROM sip_peer_config_refs WHERE id = $1",
            vec![peer.clone().into()],
        )
        .await
        .map_err(internal)?;
        if let (Some(source), Some(row)) = (inbound.source_address, &peer_row) {
            if !row.cidrs.is_empty() && !row.cidrs.iter().any(|c| cidr_contains(c, source)) {
                crate::metrics::inc(
                    "telephony_sip_ingress_rejections_total",
                    &[("reason", "SOURCE_NOT_ALLOWED")],
                );
                let incident = Incident::new(
                    "SIP_PEER_REJECTED",
                    Severity::High,
                    format!("sip-peer-source:{peer}"),
                )
                .evidence(json!({ "knownPeer": true, "reason": "SOURCE_NOT_ALLOWED" }));
                incidents::raise(&self.db, &incident, self.now())
                    .await
                    .map_err(internal)?;
                return Err(InboundRejection::SourceNotAllowed);
            }
        }
        if let Some(row) = &peer_row {
            let live = db::scalar_i64(
                &self.db,
                "SELECT count(*) FROM telephony_legs l JOIN telecom_routes r ON r.id = l.route_id \
                 WHERE r.sip_peer_config_ref_id = $1 AND l.direction = 'INBOUND' AND l.status NOT IN ('ENDED', 'FAILED')",
                vec![peer.clone().into()],
            )
            .await
            .map_err(internal)?;
            if live >= i64::from(row.max_concurrent_dialogs) {
                crate::metrics::inc(
                    "telephony_sip_ingress_rejections_total",
                    &[("reason", "PEER_CAPACITY")],
                );
                return Err(InboundRejection::PeerCapacity);
            }
        }
        let hits = self.count_ingress(&peer).await.map_err(internal)?;
        if hits > MAX_INBOUND_PER_PEER_PER_MINUTE {
            self.count_ingress_rejected(&peer).await.map_err(internal)?;
            return Err(InboundRejection::RateLimited);
        }
        // The caller hears ringing; nothing is answered (charged) yet.
        let _ = self.edge.ring(&event.channel).await;
        let correlation = cf_service_auth::CorrelationId::generate();
        // Part IV L: a synthetic test route exists only while enabled, and carries
        // synthetic DCNs only (never a relay number).
        if !self.interconnect_usable(route.interconnect()) {
            return Err(InboundRejection::InterconnectDisabled);
        }
        if route.number_kind == "DCN" {
            return self
                .accept_dcn_inbound(event, inbound, &route, &called, correlation)
                .await;
        }
        if route.interconnect() != InterconnectClass::Public {
            return Err(InboundRejection::InterconnectMismatch);
        }
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
            .map_err(relay_rejection)?;
        let caller = inbound
            .caller
            .clone()
            .ok_or(InboundRejection::AnonymousCaller)?;
        let protected = self
            .privacy
            .protect_inbound_caller(mapping.decision_id, &caller, correlation.as_str())
            .await
            .map_err(relay_rejection)?;
        self.privacy_relay_available()
            .await
            .map_err(|_| InboundRejection::Internal)?;
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
            .map_err(calling_rejection)?;
        let (call_id, call_leg_id) = accepted_refs(&accepted)?;
        let class = accepted["routeClass"]
            .as_str()
            .and_then(PublicRouteClass::parse)
            .filter(|c| matches!(c, PublicRouteClass::PstnToApp | PublicRouteClass::PstnRelay))
            .ok_or(InboundRejection::CallingInvalidResponse)?;
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

    /// Part IV L: a call to a DCN. Numbering resolves the called number to its CURRENT
    /// ACTIVE assignment and routing revision (unassigned / suspended / releasing /
    /// quarantined / unready numbers never route — and never to a former owner); the
    /// caller is protected by Privacy Relay in the scope Numbering's signed grant proves;
    /// Calling receives the grant (it verifies it with Numbering) and the opaque target —
    /// never a number.
    async fn accept_dcn_inbound(
        &self,
        event: &EdgeEvent,
        inbound: &EdgeInbound,
        route: &InboundRoute,
        called: &ExecutionNumber,
        correlation: cf_service_auth::CorrelationId,
    ) -> Result<Uuid, InboundRejection> {
        use crate::ports::{DcnInbound, PortError};
        let routing = match self
            .numbering
            .resolve_inbound_voice(called, correlation.as_str())
            .await
        {
            Ok(DcnInbound::Routed(r)) => r,
            Ok(DcnInbound::NotRouted) => {
                self.numbering_available()
                    .await
                    .map_err(|_| InboundRejection::Internal)?;
                return Err(InboundRejection::DcnNotRouted);
            }
            Ok(DcnInbound::Unavailable(reason)) => {
                self.numbering_available()
                    .await
                    .map_err(|_| InboundRejection::Internal)?;
                tracing::info!(reason = %reason, "DCN not routable");
                return Err(InboundRejection::DcnNotRoutable);
            }
            Err(PortError::Unavailable) => return Err(InboundRejection::NumberingUnavailable),
            Err(_) => return Err(InboundRejection::DcnNotRoutable),
        };
        self.numbering_available()
            .await
            .map_err(|_| InboundRejection::Internal)?;
        // The local binding and Numbering must name the same Number Resource.
        if route.number_resource_id != Some(routing.number_resource_id) {
            return Err(InboundRejection::DcnMismatch);
        }
        // A synthetic DCN arrives only on a synthetic test route, a real one only on a
        // public route (Numbering's inventory class, never the number's format).
        let interconnect = route.interconnect();
        if (interconnect == InterconnectClass::SyntheticTest) != routing.synthetic() {
            return Err(InboundRejection::InterconnectMismatch);
        }
        let caller = inbound
            .caller
            .clone()
            .ok_or(InboundRejection::AnonymousCaller)?;
        let contact_reference = self
            .privacy
            .protect_dcn_caller(&routing.grant, &caller, correlation.as_str())
            .await
            .map_err(relay_rejection)?;
        self.privacy_relay_available()
            .await
            .map_err(|_| InboundRejection::Internal)?;
        drop(caller);
        let leg_id = Uuid::new_v4();
        let offer = json!({
            "schemaVersion": "1",
            "telephonyLegId": leg_id,
            "correlationId": correlation.as_str(),
            "scope": {
                "tenantId": routing.scope.tenant_id,
                "applicationId": routing.scope.application_id,
                "environmentId": routing.scope.environment_id,
            },
            "dcnResolution": {
                "numberResourceId": routing.number_resource_id,
                "assignmentId": routing.assignment_id,
                "routingRevision": routing.routing_revision,
                "ownershipClass": routing.ownership_class,
                "target": { "kind": routing.target_kind, "ref": routing.target_ref },
                "grant": routing.grant,
            },
            "callerContactReference": contact_reference,
            "maxDurationSeconds": route.max_call_seconds,
            "interconnectClass": interconnect.as_str(),
        });
        let accepted: Value = self
            .calling
            .offer_dcn_inbound(&offer)
            .await
            .map_err(calling_rejection)?;
        drop(offer);
        let (call_id, call_leg_id) = accepted_refs(&accepted)?;
        if accepted["routeClass"] != "PSTN_TO_APP" {
            return Err(InboundRejection::CallingInvalidResponse);
        }
        let now = self.now();
        let persisted: TelephonyResult<()> = async {
            let txn = self.db.begin().await?;
            db::exec(
                &txn,
                "INSERT INTO telephony_legs (id, calling_call_id, calling_call_leg_id, tenant_id, application_id, \
                   environment_id, direction, route_class, route_id, provider_id, status, generation, contact_reference, \
                   max_duration_seconds, media_attachment, usage_status, dcn_number_resource_id, dcn_assignment_id, \
                   dcn_routing_revision, dcn_grant_id, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'INBOUND', 'PSTN_TO_APP', $7, $8, 'PLANNED', 1, $9, $10, 'APPLICATION', \
                   'PENDING', $11, $12, $13, $14, $15, $15)",
                vec![
                    leg_id.into(),
                    call_id.into(),
                    call_leg_id.into(),
                    routing.scope.tenant_id.into(),
                    routing.scope.application_id.into(),
                    routing.scope.environment_id.into(),
                    route.route_id.clone().into(),
                    route.provider_id.clone().into(),
                    contact_reference.into(),
                    route.max_call_seconds.clamp(30, 86_400).into(),
                    routing.number_resource_id.into(),
                    routing.assignment_id.into(),
                    routing.routing_revision.into(),
                    routing.grant_id.into(),
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
        crate::metrics::inc("telephony_dcn_inbound_total", &[("result", "OFFERED")]);
        Ok(leg_id)
    }

    /// A rate-limited inbound dialog: bounded metric, per-peer window of rejections and
    /// ONE incident per peer (repeats bump its occurrences; never one per packet).
    pub(crate) async fn count_ingress_rejected(&self, peer: &str) -> TelephonyResult<()> {
        crate::metrics::inc(
            "telephony_sip_ingress_rejections_total",
            &[("reason", "RATE_LIMITED")],
        );
        let now = self.now();
        let window = chrono::DateTime::<chrono::Utc>::from_timestamp(
            now.timestamp() - now.timestamp() % 60,
            0,
        )
        .unwrap_or(now);
        let rejected = db::scalar_i64(
            &self.db,
            "INSERT INTO telephony_ingress_windows (subject_kind, subject_id, window_start, hits) VALUES ('SIP_INGRESS_REJECTED', $1, $2, 1) \
             ON CONFLICT (subject_kind, subject_id, window_start) DO UPDATE SET hits = telephony_ingress_windows.hits + 1 \
             RETURNING hits",
            vec![peer.to_ascii_lowercase().into(), window.into()],
        )
        .await?;
        let incident = Incident::new(
            "SIP_INGRESS_RATE_LIMITED",
            Severity::High,
            format!("sip-ingress-rate:{}", peer.to_ascii_lowercase()),
        )
        .evidence(json!({ "rejectedThisMinute": rejected, "limitPerMinute": MAX_INBOUND_PER_PEER_PER_MINUTE }));
        incidents::raise(&self.db, &incident, now).await?;
        Ok(())
    }

    pub(crate) async fn count_ingress(&self, peer: &str) -> TelephonyResult<i64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_matching_covers_v4_v6_and_bare_addresses() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert!(cidr_contains("192.0.2.0/24", ip("192.0.2.77")));
        assert!(!cidr_contains("192.0.2.0/24", ip("192.0.3.1")));
        assert!(cidr_contains("192.0.2.7/32", ip("192.0.2.7")));
        assert!(cidr_contains("192.0.2.7", ip("192.0.2.7")));
        assert!(!cidr_contains("192.0.2.7", ip("192.0.2.8")));
        assert!(cidr_contains("0.0.0.0/0", ip("203.0.113.1")));
        assert!(cidr_contains("2001:db8::/32", ip("2001:db8:1::5")));
        assert!(!cidr_contains("2001:db8::/32", ip("2001:db9::1")));
        assert!(!cidr_contains("192.0.2.0/24", ip("::ffff:192.0.2.1")));
        assert!(!cidr_contains("garbage", ip("192.0.2.1")));
        assert!(!cidr_contains("192.0.2.0/33", ip("192.0.2.1")));
    }
}
