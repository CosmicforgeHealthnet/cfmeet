//! Part IV Workstream L: Digital Communications Numbers on Telephony's voice path.
//!
//! * Inbound: a dialled number bound locally as a DCN is resolved by Numbering PER CALL
//!   (`inbound-resolutions`, VOICE); the caller is protected by Privacy Relay under
//!   Numbering's signed INBOUND_VOICE grant; Calling receives the grant and references,
//!   never a number. Not routed / not routable / Numbering down / a local-vs-Numbering
//!   mismatch / an anonymous caller: rejected at the edge, nothing offered, no leg.
//! * Outbound: an originate command presenting a DCN carries Numbering's single-use
//!   CALLER_IDENTITY grant; Telephony consumes it for exactly this operation, checks the
//!   verified scope and number, and presents the number ONLY on an interconnect the DCN
//!   is bound to. Refusal, reuse, scope mismatch, no local binding: no provider request.
//! * Activation: Telephony acknowledges Numbering's VOICE route only while it can route
//!   the DCN locally.
//!
//! Numbering, Privacy Relay and Calling are the deterministic fakes of Telephony's ports.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{edge::ScriptedEdge, FakeDcn, Harness, RELAY_NUMBER};
use serde_json::{json, Value};
use telephony::domain::RouteCapability;
use telephony::{
    commands::originate::dcn_operation_ref,
    dcn::{self, DcnActivationError},
    domain::LegStatus,
    ports::{DcnScope, DcnVoiceRoute, InboundMapping},
    providers::ExecutionNumber,
    sip::{ChannelState, EdgeEvent, EdgeEventKind, EdgeInbound},
};
use uuid::Uuid;

const DCN: &str = "+2342010000777";
const CALLER: &str = "+2348057770001";
const PEER: &str = "peer-ng-a";

fn scope_of(h: &Harness) -> DcnScope {
    DcnScope {
        tenant_id: h.consumer.tenant_id,
        application_id: h.consumer.application_id,
        environment_id: h.consumer.environment_id,
    }
}

fn dcn_of(h: &Harness, id: Uuid, target: Option<(&str, Uuid)>) -> FakeDcn {
    FakeDcn {
        number_resource_id: id,
        assignment_id: Uuid::new_v4(),
        routing_revision: 3,
        scope: scope_of(h),
        target: target.map(|(k, r)| (k.to_string(), r)),
        unavailable: None,
        inventory_class: Some("REAL".into()),
    }
}

async fn bind_dcn(h: &Harness, number_resource_id: Uuid, route: &str, caps: &str) {
    let now = h.clock_now();
    h.sql(&format!(
        "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, number_kind, \
           number_resource_id, created_at, updated_at) \
         VALUES ('{DCN}', '{route}', ARRAY[{caps}], 'ENABLED', 'DCN', '{number_resource_id}', '{now}', '{now}')"
    ))
    .await;
}

// ---------------------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------------------

struct Edge {
    h: Harness,
    edge: ScriptedEdge,
}

impl Edge {
    async fn new() -> Option<Self> {
        let edge = ScriptedEdge::default();
        let h = Harness::with_edge(Arc::new(edge.clone())).await?;
        Some(Self { h, edge })
    }

    async fn dialog(&self, called: &str, caller: Option<&str>) -> String {
        self.dialog_via(PEER, called, caller).await
    }

    async fn dialog_via(&self, peer: &str, called: &str, caller: Option<&str>) -> String {
        let channel = format!("chan-{}", Uuid::new_v4().simple());
        self.edge.add_live(&channel, LegStatus::Ringing);
        self.h
            .svc
            .handle_edge_event(EdgeEvent {
                event_id: format!("t-{}", Uuid::new_v4().simple()),
                channel: channel.clone(),
                application_media: false,
                correlation_key: None,
                kind: EdgeEventKind::Inbound(EdgeInbound {
                    peer: Some(peer.to_string()),
                    source_address: Some("192.0.2.10".parse().unwrap()),
                    called: ExecutionNumber::parse(called),
                    caller: caller.and_then(ExecutionNumber::parse),
                    media_join_code: None,
                }),
                at: self.h.now(),
            })
            .await
            .unwrap();
        channel
    }

    async fn rejected(&self, channel: &str) -> bool {
        matches!(
            self.h.svc.edge().query(&channel.to_string()).await,
            ChannelState::Gone
        )
    }

    fn dcn_offers(&self) -> Vec<Value> {
        self.h.calling.dcn_offers.lock().unwrap().clone()
    }

    async fn inbound_legs(&self) -> i64 {
        self.h
            .scalar("SELECT count(*) FROM telephony_legs WHERE direction = 'INBOUND'")
            .await
    }
}

#[tokio::test]
async fn a_routed_dcn_call_is_offered_to_calling_with_numberings_grant_and_no_number() {
    let Some(w) = Edge::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    let extension = Uuid::new_v4();
    bind_dcn(&w.h, id, "ng-a", "'VOICE_INBOUND','VOICE_OUTBOUND'").await;
    let dcn = dcn_of(&w.h, id, Some(("EXTENSION", extension)));
    w.h.numbering.add_dcn(DCN, dcn.clone());

    let channel = w.dialog(DCN, Some(CALLER)).await;
    assert!(!w.rejected(&channel).await);
    let offers = w.dcn_offers();
    assert_eq!(offers.len(), 1);
    let offer = &offers[0];
    assert_eq!(offer["dcnResolution"]["numberResourceId"], json!(id));
    assert_eq!(
        offer["dcnResolution"]["assignmentId"],
        json!(dcn.assignment_id)
    );
    assert_eq!(offer["dcnResolution"]["routingRevision"], 3);
    assert_eq!(offer["dcnResolution"]["target"]["kind"], "EXTENSION");
    assert_eq!(offer["dcnResolution"]["target"]["ref"], json!(extension));
    assert_eq!(offer["scope"]["tenantId"], json!(w.h.consumer.tenant_id));
    let grant = offer["dcnResolution"]["grant"]
        .as_str()
        .unwrap()
        .to_string();
    // Privacy Relay protected the caller under the SAME Numbering grant; the relay
    // mapping path was never used.
    assert_eq!(*w.h.privacy.dcn_protected.lock().unwrap(), vec![grant]);
    assert!(w.h.privacy.protected.lock().unwrap().is_empty());
    assert!(w.h.calling.offers.lock().unwrap().is_empty());
    // Neither the caller nor the DCN reaches Calling.
    let text = offer.to_string();
    assert!(
        !text.contains(&CALLER[1..]) && !text.contains(&DCN[1..]),
        "{text}"
    );
    // The leg: inbound PSTN_TO_APP, ringing, with the DCN evidence (references only).
    assert_eq!(w.inbound_legs().await, 1);
    assert_eq!(
        w.h.text(
            "SELECT status || '/' || route_class || '/' || dcn_number_resource_id::text || '/' || dcn_routing_revision \
             FROM telephony_legs WHERE direction = 'INBOUND'"
        )
        .await
        .unwrap(),
        format!("RINGING/PSTN_TO_APP/{id}/3")
    );
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM telephony_legs WHERE inbound_resolution_id IS NULL AND dcn_grant_id IS NOT NULL")
            .await,
        1
    );
    let dump = w.h.dump().await;
    assert!(
        !dump.contains(&CALLER[1..]),
        "the caller is stored nowhere in Telephony"
    );
}

#[tokio::test]
async fn unroutable_dcn_calls_are_rejected_before_privacy_relay_and_calling() {
    let Some(w) = Edge::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&w.h, id, "ng-a", "'VOICE_INBOUND'").await;

    // Assigned but routed nowhere (offboarded line, not yet re-routed).
    w.h.numbering.add_dcn(DCN, dcn_of(&w.h, id, None));
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await, "not routed");

    // Suspended / quarantined / released: Numbering's UNAVAILABLE.
    let mut suspended = dcn_of(&w.h, id, Some(("PRINCIPAL", Uuid::new_v4())));
    suspended.unavailable = Some("NUMBER_NOT_ACTIVE".into());
    w.h.numbering.add_dcn(DCN, suspended);
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await, "not routable");

    // Numbering names another Number Resource than the local binding.
    w.h.numbering.add_dcn(
        DCN,
        dcn_of(&w.h, Uuid::new_v4(), Some(("PRINCIPAL", Uuid::new_v4()))),
    );
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await, "mismatch");

    // An anonymous caller.
    w.h.numbering
        .add_dcn(DCN, dcn_of(&w.h, id, Some(("PRINCIPAL", Uuid::new_v4()))));
    let c = w.dialog(DCN, None).await;
    assert!(w.rejected(&c).await, "anonymous");

    // Calling refuses (e.g. its own grant verification failed).
    *w.h.calling.dcn_refusal.lock().unwrap() = Some("NUMBER_GRANT_INVALID");
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await, "calling refused");

    assert!(w.dcn_offers().is_empty());
    assert_eq!(w.inbound_legs().await, 0);
    // Only the last two reached Privacy Relay (after a ROUTED answer with a caller).
    assert_eq!(w.h.privacy.dcn_protected.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_numbering_outage_fails_dcn_calls_closed_while_relay_numbers_keep_working() {
    let Some(w) = Edge::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&w.h, id, "ng-a", "'VOICE_INBOUND'").await;
    w.h.numbering
        .add_dcn(DCN, dcn_of(&w.h, id, Some(("PRINCIPAL", Uuid::new_v4()))));
    w.h.privacy.mappings.lock().unwrap().insert(
        RELAY_NUMBER.to_string(),
        InboundMapping {
            decision_id: Uuid::new_v4(),
            context_id: Uuid::new_v4(),
            context_version: 1,
            relay_session_id: Uuid::new_v4(),
            relay_session_version: 1,
            assignment_id: Uuid::new_v4(),
            assignment_generation: 1,
            tenant_id: w.h.consumer.tenant_id,
            application_id: w.h.consumer.application_id,
            environment_id: w.h.consumer.environment_id,
            valid_until: w.h.now() + chrono::Duration::hours(1),
        },
    );
    w.h.numbering.set_available(false);
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await);
    assert!(w.dcn_offers().is_empty());
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'NUMBERING_UNAVAILABLE' AND status = 'OPEN'"
        )
        .await,
        1
    );
    // A second failure is the same incident (no flood).
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await);
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'NUMBERING_UNAVAILABLE'"
        )
        .await,
        1
    );
    // A relay-number call does not depend on Numbering.
    let c = w.dialog(RELAY_NUMBER, Some(CALLER)).await;
    assert!(!w.rejected(&c).await);
    assert_eq!(w.h.calling.offers.lock().unwrap().len(), 1);
    // Recovery: the next DCN call resolves and the incident closes.
    w.h.numbering.set_available(true);
    let c = w.dialog(DCN, Some(CALLER)).await;
    assert!(!w.rejected(&c).await);
    assert_eq!(w.dcn_offers().len(), 1);
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'NUMBERING_UNAVAILABLE' AND status = 'OPEN'"
        )
        .await,
        0
    );
}

// ---------------------------------------------------------------------------------------
// Outbound caller identity
// ---------------------------------------------------------------------------------------

/// Originates a leg presenting the DCN under `grant` (or a freshly issued, correct grant
/// when `None`); returns (telephony leg, result code, the command body).
async fn originate_dcn(
    h: &Harness,
    id: Uuid,
    grant: impl FnOnce(&Value) -> String,
) -> (Uuid, Option<String>, Value) {
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    let token = grant(&body);
    body["presentedIdentity"] = json!({ "numberResourceId": id, "grant": token });
    let r = h
        .calling(
            Method::POST,
            "/internal/v1/legs/originate",
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let leg: Uuid = r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.run().await;
    let code = h
        .text(&format!(
            "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await;
    (leg, code, body)
}

fn op_ref(body: &Value) -> String {
    dcn_operation_ref(
        body["call"]["callLegId"].as_str().unwrap().parse().unwrap(),
        body["commandId"].as_str().unwrap().parse().unwrap(),
    )
}

fn presented(h: &Harness) -> Vec<String> {
    h.provider_a
        .presented_identities()
        .into_iter()
        .chain(h.provider_b.presented_identities())
        .map(|(_, n)| n)
        .collect()
}

#[tokio::test]
async fn a_verified_caller_identity_grant_presents_the_dcn_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&h, id, "ng-a", "'VOICE_OUTBOUND','VOICE_INBOUND'").await;
    let dcn = dcn_of(&h, id, None);
    let (leg, code, body) = originate_dcn(&h, id, |b| {
        h.numbering
            .issue_caller_identity(DCN, dcn.clone(), &op_ref(b))
    })
    .await;
    assert_eq!(
        code.as_deref(),
        Some("ACCEPTED"),
        "{}",
        h.leg_status(leg).await
    );
    assert_eq!(
        presented(&h),
        vec![DCN.to_string()],
        "the DCN is the presented identity"
    );
    // Numbering consumed the grant for exactly this operation.
    assert_eq!(
        *h.numbering.verifications.lock().unwrap(),
        vec![op_ref(&body)]
    );
    // The leg records the DCN evidence (no relay assignment), the grant is gone.
    assert_eq!(
        h.text(&format!(
            "SELECT dcn_number_resource_id::text || '/' || dcn_routing_revision || '/' || (relay_assignment_ref IS NULL) \
             FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        format!("{id}/3/true")
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_commands WHERE dcn_identity_grant IS NOT NULL")
            .await,
        0,
        "the bearer grant is scrubbed at transmit"
    );
    // The stored payload names the presented number resource, never the grant.
    let payload = h
        .text("SELECT payload::text FROM telephony_commands WHERE kind = 'ORIGINATE'")
        .await
        .unwrap();
    assert!(payload.contains(&id.to_string()));
    assert!(!payload.contains(body["presentedIdentity"]["grant"].as_str().unwrap()));
}

#[tokio::test]
async fn refused_reused_or_mismatched_grants_never_reach_the_provider() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&h, id, "ng-a", "'VOICE_OUTBOUND'").await;
    let dcn = dcn_of(&h, id, None);

    // Unknown / forged grant.
    let (_, code, _) =
        originate_dcn(&h, id, |_| "eyJhbGciOiJFZERTQSJ9.forged.c2ln".to_string()).await;
    assert_eq!(code.as_deref(), Some("CALLER_IDENTITY_DENIED"), "forged");

    // A grant for another operation (another leg's), then the reuse of a consumed one.
    let foreign = h
        .numbering
        .issue_caller_identity(DCN, dcn.clone(), "leg:other:operation");
    let (_, code, _) = originate_dcn(&h, id, |_| foreign.clone()).await;
    assert_eq!(
        code.as_deref(),
        Some("CALLER_IDENTITY_DENIED"),
        "other operation"
    );

    let mut used = String::new();
    let (_, code, _) = originate_dcn(&h, id, |b| {
        used = h
            .numbering
            .issue_caller_identity(DCN, dcn.clone(), &op_ref(b));
        used.clone()
    })
    .await;
    assert_eq!(code.as_deref(), Some("ACCEPTED"));
    h.numbering
        .grants
        .lock()
        .unwrap()
        .get_mut(&used)
        .unwrap()
        .operation_ref = "reuse".into();
    let (_, code, _) = originate_dcn(&h, id, |_| used.clone()).await;
    assert_eq!(
        code.as_deref(),
        Some("CALLER_IDENTITY_DENIED"),
        "consumed grant"
    );

    // A grant whose verified scope is another application's.
    let mut other_scope = dcn.clone();
    other_scope.scope.application_id = Uuid::new_v4();
    let (_, code, _) = originate_dcn(&h, id, |b| {
        h.numbering
            .issue_caller_identity(DCN, other_scope.clone(), &op_ref(b))
    })
    .await;
    assert_eq!(
        code.as_deref(),
        Some("CALLER_IDENTITY_DENIED"),
        "scope mismatch"
    );

    // A grant for another Number Resource than the one presented.
    let mut other_number = dcn.clone();
    other_number.number_resource_id = Uuid::new_v4();
    let (_, code, _) = originate_dcn(&h, id, |b| {
        h.numbering
            .issue_caller_identity(DCN, other_number.clone(), &op_ref(b))
    })
    .await;
    assert_eq!(
        code.as_deref(),
        Some("CALLER_IDENTITY_DENIED"),
        "number mismatch"
    );

    assert_eq!(
        presented(&h),
        vec![DCN.to_string()],
        "only the valid one was sent"
    );
}

#[tokio::test]
async fn a_dcn_is_presented_only_on_an_interconnect_it_is_bound_to() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    let dcn = dcn_of(&h, id, None);
    // Not bound at all.
    let (_, code, _) = originate_dcn(&h, id, |b| {
        h.numbering
            .issue_caller_identity(DCN, dcn.clone(), &op_ref(b))
    })
    .await;
    assert_eq!(code.as_deref(), Some("CALLER_IDENTITY_UNAVAILABLE"));
    // Bound for inbound only.
    bind_dcn(&h, id, "ng-a", "'VOICE_INBOUND'").await;
    let (_, code, _) = originate_dcn(&h, id, |b| {
        h.numbering
            .issue_caller_identity(DCN, dcn.clone(), &op_ref(b))
    })
    .await;
    assert_eq!(code.as_deref(), Some("CALLER_IDENTITY_UNAVAILABLE"));
    // Bound as a DCN of ANOTHER Number Resource on ng-b.
    let now = h.clock_now();
    h.sql(&format!(
        "UPDATE telecom_number_bindings SET status = 'DISABLED', updated_at = '{now}' WHERE public_e164 = '{DCN}'"
    ))
    .await;
    assert!(presented(&h).is_empty());
    // A relay identity of the session is never substituted for a refused DCN.
    assert!(!presented(&h).contains(&RELAY_NUMBER.to_string()));
}

#[tokio::test]
async fn a_numbering_outage_reschedules_dcn_originations_but_not_relay_ones() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&h, id, "ng-a", "'VOICE_OUTBOUND'").await;
    let dcn = dcn_of(&h, id, None);
    h.numbering.set_available(false);
    let (leg, code, _) = originate_dcn(&h, id, |b| {
        h.numbering
            .issue_caller_identity(DCN, dcn.clone(), &op_ref(b))
    })
    .await;
    assert_eq!(code.as_deref(), Some("NUMBERING_UNAVAILABLE"));
    assert_eq!(
        h.leg_status(leg).await,
        "PLANNED",
        "rescheduled, not failed"
    );
    assert!(presented(&h).is_empty());
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'NUMBERING_UNAVAILABLE'"
        )
        .await,
        1
    );
    // A relay-number origination is unaffected by the outage.
    let (relay_leg, _) = h.originate().await;
    h.run().await;
    assert_ne!(h.leg_status(relay_leg).await, "FAILED");
    assert_eq!(presented(&h), vec![RELAY_NUMBER.to_string()]);
    // Recovery: the rescheduled attempt consumes the (unconsumed) grant and sends.
    h.numbering.set_available(true);
    h.advance(30);
    h.run().await;
    assert_eq!(
        h.text(&format!(
            "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .as_deref(),
        Some("ACCEPTED")
    );
    assert_eq!(
        presented(&h),
        vec![RELAY_NUMBER.to_string(), DCN.to_string()]
    );
}

// ---------------------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn voice_readiness_is_acknowledged_only_while_telephony_can_route_the_dcn() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    let route = DcnVoiceRoute {
        route_ref: Uuid::new_v4(),
        route_revision: 2,
        status: "CONFIGURED".into(),
    };
    h.numbering
        .voice_routes
        .lock()
        .unwrap()
        .insert(id, (DCN.to_string(), Some(route.clone())));
    let db = h.svc.db();
    let numbering = h.svc.numbering().clone();

    // Not bound locally: no acknowledgement (never a fabricated success).
    let err = dcn::acknowledge(db, numbering.as_ref(), Some("ops-1"), id)
        .await
        .unwrap_err();
    assert_eq!(err, DcnActivationError::NotBoundLocally);
    assert!(h.numbering.acknowledged.lock().unwrap().is_empty());
    assert_eq!(h.svc.reconcile_dcn_routes(10).await.unwrap(), 0);

    // An operator is required; the canonical number comes from Numbering.
    assert!(matches!(
        dcn::bind(
            db,
            numbering.as_ref(),
            "",
            id,
            "ng-a",
            &["VOICE_INBOUND".into()]
        )
        .await,
        Err(DcnActivationError::InvalidOperator(_))
    ));
    let bound = dcn::bind(
        db,
        numbering.as_ref(),
        "ops-1",
        id,
        "ng-a",
        &["VOICE_INBOUND".into(), "VOICE_OUTBOUND".into()],
    )
    .await
    .unwrap();
    assert!(!bound.to_string().contains(&DCN[1..5]), "masked: {bound}");
    assert_eq!(
        h.text(&format!(
            "SELECT number_kind || '/' || number_resource_id::text FROM telecom_number_bindings WHERE public_e164 = '{DCN}'"
        ))
        .await
        .unwrap(),
        format!("DCN/{id}")
    );
    let acked = dcn::acknowledge(db, numbering.as_ref(), Some("ops-1"), id)
        .await
        .unwrap();
    assert_eq!(acked["status"], "READY");
    assert_eq!(
        *h.numbering.acknowledged.lock().unwrap(),
        vec![(id, route.route_ref, 2)]
    );
    // The reconciler re-acknowledges the configured route (idempotent at Numbering).
    assert_eq!(h.svc.reconcile_dcn_routes(10).await.unwrap(), 1);
    // A route not (yet) CONFIGURED is not acknowledged by the reconciler.
    h.numbering.voice_routes.lock().unwrap().insert(
        id,
        (
            DCN.to_string(),
            Some(DcnVoiceRoute {
                status: "REQUESTED".into(),
                ..route.clone()
            }),
        ),
    );
    assert_eq!(h.svc.reconcile_dcn_routes(10).await.unwrap(), 0);

    // Revocation at Telephony: after disable, nothing is acknowledged.
    dcn::disable(db, "ops-1", id).await.unwrap();
    assert_eq!(
        dcn::acknowledge(db, numbering.as_ref(), Some("ops-1"), id)
            .await
            .unwrap_err(),
        DcnActivationError::NotBoundLocally
    );
    // Numbering down: bind fails closed, nothing written.
    h.numbering.set_available(false);
    let other = Uuid::new_v4();
    assert_eq!(
        dcn::bind(
            db,
            numbering.as_ref(),
            "ops-1",
            other,
            "ng-a",
            &["VOICE_INBOUND".into()]
        )
        .await
        .unwrap_err(),
        DcnActivationError::NumberingUnavailable
    );
    // Every operator action is audited (bind, acknowledge, disable).
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_operator_audit WHERE action LIKE 'DCN_%'")
            .await,
        3
    );
}

#[tokio::test]
async fn a_relay_number_cannot_be_rebound_as_a_dcn() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    h.numbering
        .voice_routes
        .lock()
        .unwrap()
        .insert(id, (RELAY_NUMBER.to_string(), None));
    let numbering = h.svc.numbering().clone();
    assert_eq!(
        dcn::bind(
            h.svc.db(),
            numbering.as_ref(),
            "ops-1",
            id,
            "ng-b",
            &["VOICE_INBOUND".into()]
        )
        .await
        .unwrap_err(),
        DcnActivationError::NumberKindConflict
    );
    // And the database refuses it independently of the service check.
    let now = h.clock_now();
    assert!(h
        .try_sql(&format!(
            "INSERT INTO telecom_routes (id, provider_id, countries, capabilities, priority, status, max_concurrent_legs, \
               max_call_seconds, created_at, updated_at, health) \
             VALUES ('ng-z', 'dp-a', ARRAY['NG'], ARRAY['VOICE_INBOUND'], 40, 'ENABLED', 5, 3600, '{now}', '{now}', 'HEALTHY'); \
             INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, number_kind, number_resource_id, created_at, updated_at) \
             VALUES ('{RELAY_NUMBER}', 'ng-z', ARRAY['VOICE_INBOUND'], 'ENABLED', 'DCN', '{id}', '{now}', '{now}')"
        ))
        .await
        .is_err());
}

// ---------------------------------------------------------------------------------------
// Part IV L closure: Calling's answers, and synthetic test interconnects
// ---------------------------------------------------------------------------------------

fn offers_rejected(result: &'static str) -> u64 {
    telephony::metrics::counter("telephony_inbound_offers_total", &[("result", result)])
}

/// Step 4: Calling unreachable, Calling refusing the route, Calling denying the call and
/// an invalid Calling answer are four distinct, recorded outcomes — each rejects the
/// dialog (fail closed) and creates no leg.
#[tokio::test]
async fn calling_answers_are_classified_unavailable_refused_denied_or_invalid() {
    let Some(w) = Edge::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    bind_dcn(&w.h, id, "ng-a", "'VOICE_INBOUND'").await;
    w.h.numbering
        .add_dcn(DCN, dcn_of(&w.h, id, Some(("PRINCIPAL", Uuid::new_v4()))));
    let cases: [(&str, &'static str); 5] = [
        ("unreachable", "CALLING_UNAVAILABLE"),
        ("PUBLIC_ROUTE_DENIED", "CALLING_REFUSED"),
        ("CALL_NOT_AUTHORIZED", "CALLING_DENIED"),
        ("INTERNAL_PERMISSION_DENIED", "CALLING_DENIED"),
        ("invalid", "CALLING_INVALID_RESPONSE"),
    ];
    for (mode, result) in cases {
        *w.h.calling.available.lock().unwrap() = mode != "unreachable";
        *w.h.calling.dcn_refusal.lock().unwrap() = match mode {
            "unreachable" | "invalid" => None,
            code => Some(match code {
                "PUBLIC_ROUTE_DENIED" => "PUBLIC_ROUTE_DENIED",
                "CALL_NOT_AUTHORIZED" => "CALL_NOT_AUTHORIZED",
                _ => "INTERNAL_PERMISSION_DENIED",
            }),
        };
        *w.h.calling.dcn_answer.lock().unwrap() = (mode == "invalid")
            .then(|| json!({ "callId": "not-a-call", "routeClass": "PSTN_TO_APP" }));
        let before = offers_rejected(result);
        let c = w.dialog(DCN, Some(CALLER)).await;
        assert!(w.rejected(&c).await, "{mode}: the dialog is rejected");
        assert_eq!(
            offers_rejected(result),
            before + 1,
            "{mode} is recorded as {result}"
        );
    }
    assert_eq!(w.inbound_legs().await, 0);
    // The classifier itself (an explicit refusal is never "unavailable").
    use telephony::{inbound::calling_rejection, ports::PortError};
    assert_eq!(
        calling_rejection(PortError::Unavailable).as_str(),
        "CALLING_UNAVAILABLE"
    );
    assert_eq!(
        calling_rejection(PortError::NotFound).as_str(),
        "CALLING_REFUSED"
    );
    assert_eq!(
        calling_rejection(PortError::Denied("TELECOM_SPEND_LIMIT_EXCEEDED".into())).as_str(),
        "CALLING_REFUSED"
    );
    assert_eq!(
        calling_rejection(PortError::Denied("INTERNAL_AUTHENTICATION_FAILED".into())).as_str(),
        "CALLING_DENIED"
    );
}

/// A SYNTHETIC_TEST route (zero cost) for `provider` reached through `peer`, carrying the
/// DCN `id`.
async fn synthetic_route(h: &Harness, route: &str, peer: &str, provider: &str, id: Uuid) {
    let now = h.clock_now();
    h.sql(&format!(
        "INSERT INTO sip_peer_config_refs (id, provider_id, secret_ref, transport, allowed_source_cidrs, max_concurrent_dialogs, created_at, updated_at) \
         VALUES ('{peer}', '{provider}', 'trunk/{route}', 'UDP', ARRAY['192.0.2.0/24']::cidr[], 10, '{now}', '{now}')"
    ))
    .await;
    h.sql(&format!(
        "INSERT INTO telecom_routes (id, provider_id, countries, capabilities, priority, status, sip_peer_config_ref_id, \
           max_concurrent_legs, estimated_leg_cost_minor, cost_currency, max_call_seconds, created_at, updated_at, health, interconnect_class) \
         VALUES ('{route}', '{provider}', ARRAY['NG'], ARRAY['VOICE_OUTBOUND','VOICE_INBOUND'], 90, 'ENABLED', '{peer}', \
           10, 0, 'NGN', 3600, '{now}', '{now}', 'HEALTHY', 'SYNTHETIC_TEST')"
    ))
    .await;
    bind_dcn(h, id, route, "'VOICE_INBOUND','VOICE_OUTBOUND'").await;
}

#[tokio::test]
async fn synthetic_test_routes_carry_synthetic_dcns_only_and_only_when_enabled() {
    let Some(w) = Edge::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    synthetic_route(&w.h, "syn-a", "peer-syn-a", "sip-a", id).await;
    let mut synthetic = dcn_of(&w.h, id, Some(("PRINCIPAL", Uuid::new_v4())));
    synthetic.inventory_class = Some("SYNTHETIC".into());

    // Disabled (the default): the synthetic route does not exist for Telephony.
    w.h.numbering.add_dcn(DCN, synthetic.clone());
    let before = offers_rejected("INTERCONNECT_DISABLED");
    let c = w.dialog_via("peer-syn-a", DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await);
    assert_eq!(offers_rejected("INTERCONNECT_DISABLED"), before + 1);
    let availability =
        w.h.svc
            .availability("NG", RouteCapability::VoiceOutbound)
            .await
            .unwrap();
    assert!(
        availability["routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["routeRef"] != "syn-a"),
        "{availability}"
    );

    // Enabled: a synthetic DCN on the synthetic route is offered as SYNTHETIC_TEST.
    w.h.svc.flags.synthetic_interconnect.set(true);
    let availability =
        w.h.svc
            .availability("NG", RouteCapability::VoiceOutbound)
            .await
            .unwrap();
    let routes = availability["routes"].as_array().unwrap();
    assert!(routes
        .iter()
        .any(|r| r["routeRef"] == "syn-a" && r["interconnectClass"] == "SYNTHETIC_TEST"));
    assert!(routes
        .iter()
        .filter(|r| r["routeRef"] != "syn-a")
        .all(|r| r["interconnectClass"] == "PUBLIC"));
    let c = w.dialog_via("peer-syn-a", DCN, Some(CALLER)).await;
    assert!(!w.rejected(&c).await);
    let offers = w.dcn_offers();
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0]["interconnectClass"], "SYNTHETIC_TEST");

    // A REAL number on the synthetic route, a synthetic number on a public route: refused.
    let mut real = synthetic.clone();
    real.inventory_class = Some("REAL".into());
    w.h.numbering.add_dcn(DCN, real);
    let before = offers_rejected("INTERCONNECT_MISMATCH");
    let c = w.dialog_via("peer-syn-a", DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await, "a real number on a test route");
    w.h.numbering.add_dcn(DCN, synthetic);
    let other = Uuid::new_v4();
    let mut on_public = dcn_of(&w.h, other, Some(("PRINCIPAL", Uuid::new_v4())));
    on_public.inventory_class = Some("SYNTHETIC".into());
    const PUBLIC_DCN: &str = "+2342010000999";
    w.h.sql(&format!(
        "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, number_kind, \
           number_resource_id, created_at, updated_at) \
         VALUES ('{PUBLIC_DCN}', 'ng-a', ARRAY['VOICE_INBOUND'], 'ENABLED', 'DCN', '{other}', now(), now())"
    ))
    .await;
    w.h.numbering.add_dcn(PUBLIC_DCN, on_public);
    let c = w.dialog(PUBLIC_DCN, Some(CALLER)).await;
    assert!(w.rejected(&c).await, "a synthetic number on a public route");
    assert_eq!(offers_rejected("INTERCONNECT_MISMATCH"), before + 2);
    assert_eq!(w.dcn_offers().len(), 1);
}

/// Originates presenting the DCN over `class`; returns (leg, result code).
async fn originate_over(
    h: &Harness,
    id: Uuid,
    dcn: &FakeDcn,
    class: &str,
) -> (Uuid, Option<String>) {
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    let token = h
        .numbering
        .issue_caller_identity(DCN, dcn.clone(), &op_ref(&body));
    body["presentedIdentity"] = json!({ "numberResourceId": id, "grant": token });
    body["interconnectClass"] = json!(class);
    // Calling's candidates name both; Telephony keeps only the routes of the class.
    body["candidateRoutes"] = json!(["ng-a", "syn-a"]);
    let r = h
        .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let leg: Uuid = r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.run().await;
    let code = h
        .text(&format!(
            "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await;
    (leg, code)
}

#[tokio::test]
async fn a_synthetic_dcn_is_presented_only_over_a_synthetic_test_route() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let id = Uuid::new_v4();
    // The DCN is bound to the public route (better priority) AND to a synthetic one.
    bind_dcn(&h, id, "ng-a", "'VOICE_OUTBOUND','VOICE_INBOUND'").await;
    synthetic_route(&h, "syn-a", "peer-syn-a", "dp-a", id).await;
    let mut synthetic = dcn_of(&h, id, None);
    synthetic.inventory_class = Some("SYNTHETIC".into());

    // Disabled: refused before Numbering is asked, nothing dialled.
    let (_, code) = originate_over(&h, id, &synthetic, "SYNTHETIC_TEST").await;
    assert_eq!(code.as_deref(), Some("SYNTHETIC_INTERCONNECT_DISABLED"));
    assert!(h.numbering.verifications.lock().unwrap().is_empty());
    assert!(presented(&h).is_empty());

    h.svc.flags.synthetic_interconnect.set(true);
    // A synthetic DCN over SYNTHETIC_TEST: executed on the synthetic route only.
    let (leg, code) = originate_over(&h, id, &synthetic, "SYNTHETIC_TEST").await;
    assert_eq!(
        code.as_deref(),
        Some("ACCEPTED"),
        "{}",
        h.leg_status(leg).await
    );
    assert_eq!(
        h.text(&format!(
            "SELECT route_id FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .as_deref(),
        Some("syn-a")
    );
    assert_eq!(presented(&h), vec![DCN.to_string()]);
    // A synthetic DCN over PUBLIC, a real DCN over SYNTHETIC_TEST: refused, nothing sent.
    let (_, code) = originate_over(&h, id, &synthetic, "PUBLIC").await;
    assert_eq!(code.as_deref(), Some("INTERCONNECT_MISMATCH"));
    let mut real = synthetic.clone();
    real.inventory_class = Some("REAL".into());
    let (_, code) = originate_over(&h, id, &real, "SYNTHETIC_TEST").await;
    assert_eq!(code.as_deref(), Some("INTERCONNECT_MISMATCH"));
    assert_eq!(
        presented(&h).len(),
        1,
        "only the synthetic execution reached a provider"
    );
    // A real DCN over PUBLIC never selects the synthetic route.
    let (leg, code) = originate_over(&h, id, &real, "PUBLIC").await;
    assert_eq!(code.as_deref(), Some("ACCEPTED"));
    assert_eq!(
        h.text(&format!(
            "SELECT route_id FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .as_deref(),
        Some("ng-a")
    );
    // A SYNTHETIC_TEST origination without a presented DCN is malformed.
    let contact = h.privacy.add_contact("+2348031234567", "NG");
    let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
    body["interconnectClass"] = json!("SYNTHETIC_TEST");
    let r = h
        .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}
