//! Part IV Workstream L on Calling's side: calls to and from Digital Communications
//! Numbers (DCNs), with a scripted Numbering port.
//!
//! * Inbound (`POST /internal/v1/telephony/dcn-inbound-calls`): Calling routes ONLY to the
//!   target Numbering verified for the relayed grant — a principal (PSTN_TO_APP, no
//!   Privacy Relay session) or an enterprise extension / ring group / queue (enterprise
//!   router through an extension). A refused, substituted or unverifiable grant, an
//!   unsupported target or a Numbering outage creates no Call.
//! * Outbound (`presentedNumberResourceId` on `POST /v1/calls`): the owner of a personal
//!   number, or an ACTIVE enterprise principal reached through an organization number's
//!   voice target, may present it — organizational scope alone never suffices. Telephony
//!   receives Numbering's single-use grant for exactly that origination, never a relay
//!   identity in its place. Numbering down: DCN calls fail closed, app-to-app calls work.

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    domain::EnvironmentKind,
    numbering::{
        CallerIdentityGrant, DcnAssignmentView, DcnScope, DcnTarget, NumberingError, NumberingPort,
        VerifiedInboundGrant,
    },
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::spend::SpendPolicy,
};
use cf_service_auth::{
    CorrelationId, DelegatedPlatformContext, OutboundServiceRequest, ServiceId, ServiceTokenSigner,
};
use common::{
    app::{AppWorld, Device},
    enterprise::{id_of, EnterpriseWorld},
    privacy::FakePrivacyRelay,
    scalar_i64,
    telephony::{FakePublicMedia, FakeTelephony},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------
// Scripted Numbering
// ---------------------------------------------------------------------------------------

/// number resource -> (owner kind, owner ref, voice target)
type NumberTable = HashMap<Uuid, (String, Uuid, Option<DcnTarget>)>;

#[derive(Default)]
struct FakeNumbering {
    down: Mutex<bool>,
    /// grant token -> Numbering's verdict
    inbound: Mutex<HashMap<String, Result<VerifiedInboundGrant, String>>>,
    /// number resource -> (owner kind, owner ref, voice target)
    numbers: Mutex<NumberTable>,
    /// issued caller-identity grants: (initiating principal, number, operation ref)
    issued: Mutex<Vec<(Option<Uuid>, Uuid, String)>>,
    /// Part IV L closure: every number is SYNTHETIC inventory.
    synthetic: Mutex<bool>,
}

#[async_trait]
impl NumberingPort for FakeNumbering {
    async fn verify_inbound_voice(
        &self,
        grant: &str,
        _: &CorrelationId,
    ) -> Result<VerifiedInboundGrant, NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        match self.inbound.lock().unwrap().get(grant) {
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(reason)) => Err(NumberingError::Refused(reason.clone())),
            None => Err(NumberingError::Refused("GRANT_INVALID".into())),
        }
    }

    async fn current_assignment(
        &self,
        delegated: &DelegatedPlatformContext,
        _: &CorrelationId,
        id: Uuid,
    ) -> Result<DcnAssignmentView, NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        assert!(
            delegated.initiating_principal().is_some(),
            "the initiator is named"
        );
        let (owner_kind, owner_ref, voice) = self
            .numbers
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(NumberingError::Refused("NOT_FOUND".into()))?;
        Ok(DcnAssignmentView {
            number_resource_id: id,
            assignment_id: Uuid::new_v4(),
            ownership_class: if owner_kind == "PRINCIPAL" {
                "PERSONAL"
            } else {
                "ORGANIZATION"
            }
            .into(),
            owner_kind,
            owner_ref,
            voice,
            routing_revision: 2,
            number_active: true,
            voice_outbound: true,
            synthetic: *self.synthetic.lock().unwrap(),
        })
    }

    async fn presentation_grant(
        &self,
        delegated: &DelegatedPlatformContext,
        _: &CorrelationId,
        id: Uuid,
        operation_ref: &str,
    ) -> Result<CallerIdentityGrant, NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        self.issued.lock().unwrap().push((
            delegated.initiating_principal().map(|p| p.principal_id),
            id,
            operation_ref.to_string(),
        ));
        Ok(CallerIdentityGrant {
            grant: format!("eyJhbGciOiJFZERTQSJ9.{}.c2ln", Uuid::new_v4().simple()),
            grant_id: Uuid::new_v4(),
        })
    }
}

// ---------------------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------------------

struct World {
    e: EnterpriseWorld,
    numbering: Arc<FakeNumbering>,
    telephony: Arc<FakeTelephony>,
    privacy: Arc<FakePrivacyRelay>,
    callee: Device,
    caller: Device,
}

async fn world() -> Option<World> {
    world_in(EnvironmentKind::Production, false).await
}

/// A world in an environment of `kind`; `synthetic`: Calling's synthetic test
/// interconnect switch (Part IV L closure).
async fn world_in(kind: EnvironmentKind, synthetic: bool) -> Option<World> {
    let numbering = Arc::new(FakeNumbering::default());
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let media = Arc::new(FakePublicMedia::default());
    let n = numbering.clone();
    let a = AppWorld::with_public_network_in(
        kind,
        privacy.clone(),
        telephony.clone(),
        Some(media),
        move |runtime| {
            runtime.numbering = n;
            runtime.public_network.synthetic_interconnect = synthetic;
        },
    )
    .await?;
    privacy.allow(a.w.initiator.id().as_uuid());
    let scope = a.w.scope();
    let env = a.w.consumer.env(kind).id.as_uuid();
    calling::routing::store::store_policy(
        a.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env,
        &RoutingPolicy {
            allowed_route_classes: vec![
                RoutingClass::AppRtc,
                RoutingClass::PstnToApp,
                RoutingClass::AppToPstn,
                RoutingClass::EnterpriseInternal,
            ],
            public_calling_enabled: true,
            destinations: DestinationPolicy {
                allow: vec!["NG".into()],
                deny: vec![],
                default: CountryDecision::Deny,
            },
            ..RoutingPolicy::default()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    calling::telephony::spend::store_policy(
        a.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: env,
        },
        &SpendPolicy {
            currency: "NGN".into(),
            leg_allowance_minor: 2_000,
            max_leg_seconds: 3600,
            daily_limit_minor: 100_000,
            monthly_limit_minor: 1_000_000,
            max_concurrent_legs: 10,
            max_originations_per_minute: 30,
            max_per_party_per_hour: 10,
            max_per_principal_per_hour: None,
            max_per_service_account_per_hour: None,
            max_per_context_per_hour: None,
            enabled: true,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let callee = a.device(&a.w.recipient, "callee-main").await;
    let caller = a.device(&a.w.initiator, "caller-main").await;
    let credential = a.w.token.credential.id.as_uuid();
    use sea_orm::ConnectionTrait;
    a.h()
        .conn()
        .execute_unprepared(&format!(
            "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, service_account_id, \
               credential_id, enabled, revision, created_at, updated_at) \
             SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, now(), now() \
             FROM platform_api_credentials WHERE id = '{credential}'"
        ))
        .await
        .unwrap();
    let e = EnterpriseWorld::around(a).await;
    Some(World {
        e,
        numbering,
        telephony,
        privacy,
        callee,
        caller,
    })
}

impl World {
    fn a(&self) -> &AppWorld {
        &self.e.a
    }

    fn scope(&self) -> DcnScope {
        let s = self.a().w.scope();
        DcnScope {
            tenant_id: s.tenant_id().as_uuid(),
            application_id: s.application_id().as_uuid(),
            environment_id: s.environment_id().as_uuid(),
        }
    }

    /// Registers Numbering's verdict for a fresh grant routing a DCN to (kind, target).
    fn grant(&self, kind: &str, target: Uuid) -> (String, VerifiedInboundGrant) {
        let token = format!("eyJhbGciOiJFZERTQSJ9.in{}.c2ln", Uuid::new_v4().simple());
        let v = VerifiedInboundGrant {
            grant_id: Uuid::new_v4(),
            number_resource_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            routing_revision: 4,
            scope: self.scope(),
            target: DcnTarget {
                kind: kind.into(),
                target_ref: target,
            },
            synthetic: *self.numbering.synthetic.lock().unwrap(),
        };
        self.numbering
            .inbound
            .lock()
            .unwrap()
            .insert(token.clone(), Ok(v.clone()));
        (token, v)
    }

    fn offer(&self, telephony_leg: Uuid, token: &str, v: &VerifiedInboundGrant) -> Value {
        let s = v.scope;
        json!({
            "schemaVersion": "1",
            "telephonyLegId": telephony_leg,
            "correlationId": CorrelationId::generate().as_str(),
            "scope": {
                "tenantId": s.tenant_id,
                "applicationId": s.application_id,
                "environmentId": s.environment_id,
            },
            "dcnResolution": {
                "numberResourceId": v.number_resource_id,
                "assignmentId": v.assignment_id,
                "routingRevision": v.routing_revision,
                "ownershipClass": "ORGANIZATION",
                "target": { "kind": v.target.kind, "ref": v.target.target_ref },
                "grant": token,
            },
            "callerContactReference": Uuid::new_v4(),
            "maxDurationSeconds": 3600,
        })
    }

    async fn send(&self, signer: Arc<ServiceTokenSigner>, body: Value) -> TestResponse {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
            .unwrap();
        let headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.a()
            .h()
            .request_with(
                Method::POST,
                "/internal/v1/telephony/dcn-inbound-calls",
                None,
                Some(body),
                &refs,
            )
            .await
    }

    async fn offer_as_telephony(&self, body: Value) -> TestResponse {
        self.send(self.a().h().peers.telephony.clone(), body).await
    }

    async fn calls(&self) -> i64 {
        scalar_i64(self.a().h().conn(), "SELECT count(*) FROM calls").await
    }

    async fn dcn_call(&self, number: Uuid, key: &str) -> TestResponse {
        let contact = self.privacy.add_contact("NG");
        self.a()
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.a().w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": contact,
                    "privacyRelaySessionId": Uuid::new_v4(),
                    "presentedNumberResourceId": number,
                }),
                &[("idempotency-key", key)],
            )
            .await
    }

    fn own_number(&self, owner_kind: &str, owner: Uuid, voice: Option<(&str, Uuid)>) -> Uuid {
        let id = Uuid::new_v4();
        self.numbering.numbers.lock().unwrap().insert(
            id,
            (
                owner_kind.into(),
                owner,
                voice.map(|(k, r)| DcnTarget {
                    kind: k.into(),
                    target_ref: r,
                }),
            ),
        );
        id
    }
}

// ---------------------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_dcn_routed_to_a_principal_rings_it_without_a_privacy_relay_session() {
    let Some(x) = world().await else { return };
    let (token, v) = x.grant("PRINCIPAL", x.a().w.recipient.id().as_uuid());
    let t = Uuid::new_v4();
    let r = x.offer_as_telephony(x.offer(t, &token, &v)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["routeClass"], "PSTN_TO_APP");
    let call: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    assert_eq!(x.a().state(call).await.0, CallState::Ringing);
    assert!(x
        .a()
        .invitation_for(call, x.callee.endpoint)
        .await
        .is_some());
    let conn = x.a().h().conn();
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT count(*) FROM calls WHERE id = '{call}' AND origin = 'PUBLIC_INBOUND' \
                 AND privacy_relay_ref IS NULL AND communication_context_ref IS NULL"
            )
        )
        .await,
        1
    );
    // The authorization names Numbering's verified routing (references only).
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT count(*) FROM call_authorizations WHERE call_id = '{call}' \
                 AND policy_version = 'DCN_ROUTING_V1' \
                 AND policy_snapshot ->> 'dcn_number_resource_id' = '{}' \
                 AND policy_snapshot ->> 'dcn_grant_id' = '{}'",
                v.number_resource_id, v.grant_id
            )
        )
        .await,
        1
    );
    // The public leg is bound to the Telephony leg; a replay is the same Call.
    let again = x.offer_as_telephony(x.offer(t, &token, &v)).await;
    assert_eq!(again.json["callId"], json!(call));
    // The grant is never persisted by Calling.
    let dump = common::dump_database(conn).await;
    assert!(!dump.contains(&token), "the bearer grant is stored nowhere");
}

#[tokio::test]
async fn a_dcn_routed_to_an_enterprise_extension_or_group_enters_the_enterprise_router() {
    let Some(x) = world().await else { return };
    let member = x.a().w.recipient.id().as_uuid();
    let ext = x.e.extension("410", "PRINCIPAL", member).await;
    let (token, v) = x.grant("ENTERPRISE_EXTENSION", id_of(&ext, "extensionId"));
    let r = x
        .offer_as_telephony(x.offer(Uuid::new_v4(), &token, &v))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let call: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    assert!(x
        .a()
        .invitation_for(call, x.callee.endpoint)
        .await
        .is_some());
    assert_eq!(
        x.e.count(&format!(
            "SELECT count(*) FROM enterprise_routing_steps WHERE call_id = '{call}' AND reason = 'DCN_DIALLED'"
        ))
        .await,
        1
    );
    assert_eq!(
        x.e.count(&format!(
            "SELECT count(*) FROM call_authorizations WHERE call_id = '{call}' \
             AND policy_snapshot ->> 'dcn_number_resource_id' = '{}'",
            v.number_resource_id
        ))
        .await,
        1
    );

    // A ring group is entered through the extension that publishes it...
    let group =
        x.e.create(
            "/v1/enterprise/ring-groups",
            json!({ "name": "Front desk", "strategy": "SIMULTANEOUS",
                    "memberRingSeconds": 20, "groupTimeoutSeconds": 40 }),
        )
        .await;
    let group_id = id_of(&group, "ringGroupId");
    let (token, v) = x.grant("ENTERPRISE_RING_GROUP", group_id);
    let before = x.calls().await;
    let r = x
        .offer_as_telephony(x.offer(Uuid::new_v4(), &token, &v))
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "no entry extension: {}",
        r.text
    );
    assert_eq!(x.calls().await, before);
    x.e.extension("420", "RING_GROUP", group_id).await;
    let r = x
        .offer_as_telephony(x.offer(Uuid::new_v4(), &token, &v))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn refused_substituted_or_unverifiable_grants_create_no_call() {
    let Some(x) = world().await else { return };
    let recipient = x.a().w.recipient.id().as_uuid();
    let telephony = x.a().h().peers.telephony.clone();

    // Numbering refuses the grant (revoked / stale revision / expired).
    let (token, v) = x.grant("PRINCIPAL", recipient);
    x.numbering
        .inbound
        .lock()
        .unwrap()
        .insert(token.clone(), Err("STALE_ROUTING_REVISION".into()));
    let r = x
        .offer_as_telephony(x.offer(Uuid::new_v4(), &token, &v))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED");

    // The relayed target is not the verified one (a substituted target is refused,
    // never "corrected").
    let (token, v) = x.grant("PRINCIPAL", recipient);
    let mut offer = x.offer(Uuid::new_v4(), &token, &v);
    offer["dcnResolution"]["target"]["ref"] = json!(x.a().w.initiator.id());
    let r = x.offer_as_telephony(offer).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // ...nor another scope or routing revision.
    let mut offer = x.offer(Uuid::new_v4(), &token, &v);
    offer["dcnResolution"]["routingRevision"] = json!(5);
    assert_eq!(
        x.offer_as_telephony(offer).await.status,
        StatusCode::FORBIDDEN
    );

    // A target Calling cannot route a DCN to.
    let (token, v) = x.grant("VOICEMAIL", Uuid::new_v4());
    let r = x
        .offer_as_telephony(x.offer(Uuid::new_v4(), &token, &v))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED");

    // Numbering down: fail closed (503), nothing created.
    let (token, v) = x.grant("PRINCIPAL", recipient);
    *x.numbering.down.lock().unwrap() = true;
    let r = x
        .offer_as_telephony(x.offer(Uuid::new_v4(), &token, &v))
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    *x.numbering.down.lock().unwrap() = false;

    // Only Telephony may offer.
    let r = x
        .send(
            x.a().h().peers.privacy_relay.clone(),
            x.offer(Uuid::new_v4(), &token, &v),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // Malformed: an unknown field.
    let mut offer = x.offer(Uuid::new_v4(), &token, &v);
    offer["e164"] = json!("+2348030000000");
    assert_eq!(
        x.send(telephony, offer).await.status,
        StatusCode::BAD_REQUEST
    );

    assert_eq!(x.calls().await, 0);
}

// ---------------------------------------------------------------------------------------
// Outbound caller identity
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_owner_presents_a_personal_dcn_under_a_single_use_grant() {
    let Some(x) = world().await else { return };
    let me = x.a().w.initiator.id().as_uuid();
    let number = x.own_number("PRINCIPAL", me, Some(("PRINCIPAL", me)));
    let r = x.dcn_call(number, "dcn-own-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    let command = &originates[0].1;
    assert_eq!(
        command["presentedIdentity"]["numberResourceId"],
        json!(number)
    );
    assert!(command["presentedIdentity"]["grant"]
        .as_str()
        .unwrap()
        .contains('.'));
    assert!(command.get("presentedNumberResourceId").is_none());
    // The grant was issued for the initiator, for exactly this origination.
    let issued = x.numbering.issued.lock().unwrap().clone();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].0, Some(me));
    assert_eq!(
        issued[0].2,
        format!(
            "leg:{}:{}",
            command["call"]["callLegId"]
                .as_str()
                .unwrap()
                .replace('-', ""),
            command["commandId"].as_str().unwrap().replace('-', "")
        )
    );
    // No relay identity was lent for this call.
    assert!(x.privacy.relay_identities.lock().unwrap().is_empty());
    // The stored command never carries the grant.
    let dump = common::dump_database(x.a().h().conn()).await;
    assert!(!dump.contains(command["presentedIdentity"]["grant"].as_str().unwrap()));
}

#[tokio::test]
async fn organizational_scope_alone_never_entitles_a_member_to_present_a_number() {
    let Some(x) = world().await else { return };
    let me = x.a().w.initiator.id().as_uuid();
    let colleague = x.a().w.recipient.id().as_uuid();
    let org = Uuid::new_v4();
    // Someone else's personal number.
    let theirs = x.own_number("PRINCIPAL", colleague, None);
    let r = x.dcn_call(theirs, "dcn-deny-1").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED");
    // An organization number routed to a colleague's extension.
    let ext = x.e.extension("510", "PRINCIPAL", colleague).await;
    let main = x.own_number(
        "ORGANIZATION",
        org,
        Some(("ENTERPRISE_EXTENSION", id_of(&ext, "extensionId"))),
    );
    assert_eq!(
        x.dcn_call(main, "dcn-deny-2").await.status,
        StatusCode::FORBIDDEN
    );
    // An organization number with no voice target, and an unknown number.
    let unrouted = x.own_number("ORGANIZATION", org, None);
    assert_eq!(
        x.dcn_call(unrouted, "dcn-deny-3").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        x.dcn_call(Uuid::new_v4(), "dcn-deny-4").await.status,
        StatusCode::FORBIDDEN
    );
    assert!(x.telephony.originates().is_empty());
    assert!(x.numbering.issued.lock().unwrap().is_empty());

    // A ring group's member may present the group's number; removed, no longer.
    let group =
        x.e.create(
            "/v1/enterprise/ring-groups",
            json!({ "name": "Sales desk", "strategy": "SIMULTANEOUS",
                    "memberRingSeconds": 20, "groupTimeoutSeconds": 40 }),
        )
        .await;
    let group_id = id_of(&group, "ringGroupId");
    let sales = x.own_number(
        "ORGANIZATION",
        org,
        Some(("ENTERPRISE_RING_GROUP", group_id)),
    );
    assert_eq!(
        x.dcn_call(sales, "dcn-group-1").await.status,
        StatusCode::FORBIDDEN
    );
    let set = |members: Value, version: i64| {
        let e = &x.e;
        async move {
            let r = e
                .admin(
                    Method::PUT,
                    &format!("/v1/enterprise/ring-groups/{group_id}/members"),
                    Some(json!({ "expectedVersion": version, "members": members })),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        }
    };
    set(json!([{ "type": "PRINCIPAL", "id": me.to_string() }]), 1).await;
    let r = x.dcn_call(sales, "dcn-group-2").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    set(
        json!([{ "type": "PRINCIPAL", "id": colleague.to_string() }]),
        2,
    )
    .await;
    assert_eq!(
        x.dcn_call(sales, "dcn-group-3").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(x.telephony.originates().len(), 1);
}

#[tokio::test]
async fn a_numbering_outage_fails_dcn_calls_closed_but_not_app_calls() {
    let Some(x) = world().await else { return };
    let me = x.a().w.initiator.id().as_uuid();
    let number = x.own_number("PRINCIPAL", me, None);
    *x.numbering.down.lock().unwrap() = true;
    let r = x.dcn_call(number, "dcn-down-1").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_UNAVAILABLE");
    assert!(x.telephony.originates().is_empty());
    // App-to-app calling never depends on Numbering.
    let r = x
        .a()
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": me,
                "initiatorEndpointId": x.caller.endpoint,
                "targetPrincipalIds": [x.a().w.recipient.id()],
            }),
            &[("idempotency-key", "app-during-outage")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // presentedNumberResourceId is only meaningful with a protected-contact target.
    let r = x
        .a()
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": me,
                "initiatorEndpointId": x.caller.endpoint,
                "targetPrincipalIds": [x.a().w.recipient.id()],
                "presentedNumberResourceId": number,
            }),
            &[("idempotency-key", "app-with-dcn")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

// ---------------------------------------------------------------------------------------
// Part IV L closure: the synthetic test interconnect at the route-policy boundary
// ---------------------------------------------------------------------------------------

fn synthetic_offer(
    x: &World,
    kind: &str,
    target: Uuid,
    class: &str,
) -> (Value, VerifiedInboundGrant) {
    let (token, v) = x.grant(kind, target);
    let mut offer = x.offer(Uuid::new_v4(), &token, &v);
    offer["interconnectClass"] = json!(class);
    (offer, v)
}

#[tokio::test]
async fn a_synthetic_dcn_over_the_synthetic_test_interconnect_rings_the_principal_in_a_sandbox() {
    let Some(x) = world_in(EnvironmentKind::Sandbox, true).await else {
        return;
    };
    *x.numbering.synthetic.lock().unwrap() = true;
    let (offer, v) = synthetic_offer(
        &x,
        "PRINCIPAL",
        x.a().w.recipient.id().as_uuid(),
        "SYNTHETIC_TEST",
    );
    let r = x.offer_as_telephony(offer).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["routeClass"], "PSTN_TO_APP");
    let call: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    assert_eq!(x.a().state(call).await.0, CallState::Ringing);
    assert!(x
        .a()
        .invitation_for(call, x.callee.endpoint)
        .await
        .is_some());
    let conn = x.a().h().conn();
    // The route records the synthetic test interconnect; the authorization Numbering's
    // verified routing; a spend reservation still covers the (zero-cost) leg.
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT count(*) FROM call_routes WHERE call_id = '{call}' \
                 AND route_plan ->> 'interconnectClass' = 'SYNTHETIC_TEST'"
            )
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT count(*) FROM call_authorizations WHERE call_id = '{call}' \
                 AND policy_snapshot ->> 'dcn_grant_id' = '{}'",
                v.grant_id
            )
        )
        .await,
        1
    );
}

#[tokio::test]
async fn the_synthetic_test_interconnect_needs_every_condition_and_never_mixes_with_real_ones() {
    // Sandbox with the switch OFF: a synthetic route is refused; a real route is a paid
    // public call, which a sandbox can never make.
    let Some(off) = world_in(EnvironmentKind::Sandbox, false).await else {
        return;
    };
    let callee = off.a().w.recipient.id().as_uuid();
    *off.numbering.synthetic.lock().unwrap() = true;
    let (offer, _) = synthetic_offer(&off, "PRINCIPAL", callee, "SYNTHETIC_TEST");
    let r = off.offer_as_telephony(offer).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED");
    *off.numbering.synthetic.lock().unwrap() = false;
    let (offer, _) = synthetic_offer(&off, "PRINCIPAL", callee, "PUBLIC");
    let r = off.offer_as_telephony(offer).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(
        r.json["error"]["code"], "CALL_NOT_AUTHORIZED",
        "LIVE_TELECOM_NOT_ALLOWED"
    );
    assert_eq!(off.calls().await, 0);

    // Sandbox with the switch ON: a synthetic number over a PUBLIC route, a real number
    // over the synthetic route, and a real number over a real route (paid) are refused.
    let Some(on) = world_in(EnvironmentKind::Sandbox, true).await else {
        return;
    };
    let callee = on.a().w.recipient.id().as_uuid();
    for (synthetic, class) in [
        (true, "PUBLIC"),
        (false, "SYNTHETIC_TEST"),
        (false, "PUBLIC"),
    ] {
        *on.numbering.synthetic.lock().unwrap() = synthetic;
        let (offer, _) = synthetic_offer(&on, "PRINCIPAL", callee, class);
        let r = on.offer_as_telephony(offer).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{synthetic}/{class}: {}",
            r.text
        );
    }
    assert_eq!(on.calls().await, 0);

    // PRODUCTION (live telecom allowed) never uses the synthetic test interconnect, even
    // with the switch on.
    let Some(prod) = world_in(EnvironmentKind::Production, true).await else {
        return;
    };
    *prod.numbering.synthetic.lock().unwrap() = true;
    let (offer, _) = synthetic_offer(
        &prod,
        "PRINCIPAL",
        prod.a().w.recipient.id().as_uuid(),
        "SYNTHETIC_TEST",
    );
    let r = prod.offer_as_telephony(offer).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(prod.calls().await, 0);
}

fn routes_with_synthetic(
) -> calling::routing::engine::Evidence<Vec<calling::routing::engine::CandidateRoute>> {
    use calling::routing::engine::{CandidateRoute, Evidence, InterconnectClass, RouteHealth};
    let route = |r: &str, cost: i64, class: InterconnectClass| CandidateRoute {
        route_ref: r.into(),
        health: RouteHealth::Healthy,
        capacity_available: Evidence::Known(true),
        estimated_leg_cost_minor: Evidence::Known(cost),
        quality_permille: Evidence::Unknown,
        interconnect_class: class,
    };
    Evidence::Known(vec![
        route("ng-a", 100, InterconnectClass::Public),
        route("syn-a", 0, InterconnectClass::SyntheticTest),
    ])
}

#[tokio::test]
async fn a_synthetic_dcn_is_presented_over_the_synthetic_test_interconnect_only() {
    let Some(x) = world_in(EnvironmentKind::Sandbox, true).await else {
        return;
    };
    x.telephony.set_routes(routes_with_synthetic());
    let me = x.a().w.initiator.id().as_uuid();
    *x.numbering.synthetic.lock().unwrap() = true;
    let number = x.own_number("PRINCIPAL", me, Some(("PRINCIPAL", me)));
    let r = x.dcn_call(number, "dcn-syn-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    let command = &originates[0].1;
    // Telephony is told the class and offered the synthetic route only.
    assert_eq!(command["interconnectClass"], "SYNTHETIC_TEST");
    assert_eq!(command["candidateRoutes"], json!(["syn-a"]));
    assert_eq!(
        command["presentedIdentity"]["numberResourceId"],
        json!(number)
    );
    assert_eq!(x.numbering.issued.lock().unwrap().len(), 1);

    // Not entitled: refused before any route or grant (organizational scope never suffices).
    let other = x.own_number("PRINCIPAL", x.a().w.recipient.id().as_uuid(), None);
    let r = x.dcn_call(other, "dcn-syn-2").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED");
    // No synthetic route available: refused, never "upgraded" to a real route.
    x.telephony.set_routes(common::telephony::healthy_routes());
    let r = x.dcn_call(number, "dcn-syn-3").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(x.telephony.originates().len(), 1);
}

#[tokio::test]
async fn sandbox_callers_still_cannot_use_paid_public_routes() {
    // Switch off: a synthetic number is an ordinary paid public call in a sandbox.
    let Some(off) = world_in(EnvironmentKind::Sandbox, false).await else {
        return;
    };
    off.telephony.set_routes(routes_with_synthetic());
    let me = off.a().w.initiator.id().as_uuid();
    *off.numbering.synthetic.lock().unwrap() = true;
    let number = off.own_number("PRINCIPAL", me, Some(("PRINCIPAL", me)));
    let r = off.dcn_call(number, "dcn-sbx-1").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED");
    // Switch on: a REAL number presented from a sandbox is still a paid public call.
    let Some(on) = world_in(EnvironmentKind::Sandbox, true).await else {
        return;
    };
    on.telephony.set_routes(routes_with_synthetic());
    let me = on.a().w.initiator.id().as_uuid();
    let real = on.own_number("PRINCIPAL", me, Some(("PRINCIPAL", me)));
    let r = on.dcn_call(real, "dcn-sbx-2").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED");
    assert!(off.telephony.originates().is_empty());
    assert!(on.telephony.originates().is_empty());
}
