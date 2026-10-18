//! Part IV Workstream K: routing grants — inbound voice / SMS resolution from canonical
//! state, single-use caller-identity and SMS-sender grants (no spoofing: own ACTIVE
//! numbers, usable capability, permitted principal), signed revision-bound grants with a
//! dedicated key, authoritative verification with immediate revocation (routing change,
//! suspension, transfer, capability revocation), expiry, forgery, single use under
//! concurrency, protected-runtime rejection of synthetic inventory, fail-closed
//! configuration, database guards, contracts and migration rollback.
mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::{
    number_grant::{NumberGrantKind, NumberGrantSigner},
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind, KeyId,
    RuntimeEnvironment, ServiceId, ServiceSigningKey,
};
use common::{
    acquisition as ax, catalog, contracts::conforms, exec, kyc as fx, providers as px, scalar,
    scope, strings, Harness, Reply,
};
use numbering::{
    acquisition::{
        assignments::{AssignmentBasis, AssignmentRequest, TransferRequest},
        provisioning::ProvisionRequest,
        reservations::{customer_actor, ReservationRequest},
        search::{SearchOutcome, SearchRequest},
        Acquisition, AcquisitionSettings,
    },
    domain::RequestContext,
    grants::{reason, DelegatedScope, InboundResolution, RoutingGrants},
    identity::PrincipalStatus as P,
    numbers::{
        acquisitions::RouteFamily,
        assignments::AssignmentRow,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::SubjectKind,
        routing::RoutingTargets,
        Actor, Capability as C, EnvironmentKind, NumberType, OwnerScope, OwnershipClass as O,
        SmsTargetKind as Sms, VoiceTargetKind as V,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

const COMMON: &str = "numbering/common/v1.schema.json#/$defs/";
const GRANT: &str = "numbering/routing-grant/v1.schema.json#/$defs/";
const GEO: [C; 3] = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];
const RESOLVE: &str = "/internal/v1/routing/inbound-resolutions";
const RESOLVE_SMS: &str = "/internal/v1/routing/sms-destination-resolutions";
const CALLER: &str = "/internal/v1/routing/caller-identity-grants";
const SENDER: &str = "/internal/v1/routing/sms-sender-grants";
const VERIFY: &str = "/internal/v1/routing/grant-verifications";

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn standing() -> Standing {
    Standing {
        active: true,
        first_party: false,
        live_telecom_allowed: false,
    }
}

fn key() -> String {
    format!("key-{}", Uuid::new_v4().simple())
}

fn op() -> String {
    format!("call-{}", Uuid::new_v4().simple())
}

fn targets(voice: (V, Option<Uuid>), sms: (Sms, Option<Uuid>)) -> RoutingTargets {
    RoutingTargets { voice, sms }
}

fn delegated(s: &OwnerScope, principal: Option<Uuid>) -> DelegatedPlatformContext {
    let d = DelegatedPlatformContext::new(
        s.tenant_id,
        s.application_id,
        s.environment_id,
        ApplicationEnvironmentKind::Sandbox,
        s.service_account_id,
    );
    match principal {
        Some(p) => d.with_initiating_principal(p, DelegatedPrincipalKind::EnterpriseMember),
        None => d,
    }
}

struct K {
    h: Harness,
    a: Acquisition,
    dir: Arc<ax::Principals>,
    runner: numbering::infra::jobs::JobRunner,
    clock: cf_service_auth::ManualClock,
}

async fn setup() -> Option<K> {
    let h = Harness::new().await?;
    let (_, vault) = fx::vault();
    let kyc = fx::service(
        &h,
        vault,
        Arc::new(numbering::numbers::kyc::verifier::DeterministicKycVerifier::new()),
    );
    let h = h.with_kyc(kyc, EligibilityPolicy::default());
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    let (h, clock) = h.with_grants();
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let dir = Arc::new(ax::Principals::default());
    let a = ax::of(&h, &p, AcquisitionSettings::default()).with_principals(dir.clone());
    let runner = ax::runner(&h, &p, &a);
    Some(K {
        h,
        a,
        dir,
        runner,
        clock,
    })
}

impl K {
    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    async fn id_of(&self, s: &OwnerScope, external_ref: &str) -> Uuid {
        match self
            .a
            .search(
                &ctx(),
                s,
                false,
                &SearchRequest {
                    country: "US".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        {
            SearchOutcome::Page(_) => {}
            SearchOutcome::Throttled { .. } => panic!("throttled"),
        }
        Uuid::parse_str(
            &strings(
                self.h.conn(),
                &format!("SELECT number_resource_id::text FROM number_provider_inventory_refs WHERE external_ref = '{external_ref}'"),
            )
            .await
            .pop()
            .unwrap(),
        )
        .unwrap()
    }

    async fn decide(
        &self,
        s: &OwnerScope,
        subject: (SubjectKind, Uuid),
        class: O,
        number_type: NumberType,
        caps: &[C],
        number: Uuid,
    ) -> Uuid {
        let d = self
            .h
            .numbers
            .evaluate_eligibility(
                &ctx(),
                s,
                standing(),
                &EligibilityPolicy::default(),
                EligibilityRequest {
                    subject_kind: subject.0,
                    subject_ref: subject.1,
                    country: "US".into(),
                    number_type,
                    ownership_class: class,
                    capabilities: caps.to_vec(),
                    number_resource_id: Some(number),
                    provider_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(d.outcome, "ELIGIBLE", "{:?}", d.reasons);
        d.id
    }

    /// An ACTIVE assignment of `class` on a fresh number, every family of `caps` ready.
    #[allow(clippy::too_many_arguments)]
    async fn active(
        &self,
        s: &OwnerScope,
        external_ref: &str,
        number_type: NumberType,
        caps: &[C],
        class: O,
        owner: Option<Uuid>,
        routing: RoutingTargets,
    ) -> (Uuid, AssignmentRow) {
        let id = self.id_of(s, external_ref).await;
        let org = (SubjectKind::Organization, s.tenant_id);
        let hold_class = if owner.is_some() {
            O::Organization
        } else {
            class
        };
        let hold = self.decide(s, org, hold_class, number_type, caps, id).await;
        let r = self
            .a
            .create_reservation(
                &ctx(),
                s,
                ReservationRequest {
                    number_resource_id: id,
                    capabilities: caps.to_vec(),
                    hold_seconds: Some(3_600),
                    eligibility_decision_id: Some(hold),
                },
                &key(),
            )
            .await
            .unwrap()
            .reservation;
        self.drain().await;
        self.a
            .provision(
                &ctx(),
                s,
                standing(),
                ProvisionRequest {
                    reservation_id: r.id,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: None,
                },
                &key(),
            )
            .await
            .unwrap();
        self.drain().await;
        let acq = self
            .h
            .numbers
            .acquisition_of_reservation(r.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acq.status, "PROVISIONED");
        let subject = match owner {
            Some(p) => (SubjectKind::Individual, p),
            None => org,
        };
        let decision = self.decide(s, subject, class, number_type, caps, id).await;
        let a = self
            .a
            .create_assignment(
                &ctx(),
                s,
                standing(),
                AssignmentRequest {
                    basis: AssignmentBasis::Acquisition(acq.id),
                    ownership_class: class,
                    owner_principal_id: owner,
                    targets: routing,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: Some(decision),
                    pool_id: None,
                },
                &key(),
            )
            .await
            .unwrap()
            .assignment;
        self.a
            .configure_routes(&ctx(), s, acq.id, caps, &key())
            .await
            .unwrap();
        self.drain().await;
        for r in self.h.numbers.current_routes(id).await.unwrap() {
            let peer = if r.family == "VOICE" {
                &self.h.peers.telephony
            } else {
                &self.h.peers.sms_gateway
            };
            let reply = self
                .h
                .internal_json(
                    peer,
                    Method::POST,
                    &format!("/internal/v1/number-resources/{id}/route-readiness"),
                    json!({ "family": r.family, "routeRef": r.route_ref, "routeRevision": r.route_revision }),
                )
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
        }
        let a = self
            .a
            .activate_assignment(&ctx(), s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
        assert_eq!(a.status, "ACTIVE");
        (id, a)
    }

    async fn e164(&self, id: Uuid) -> String {
        self.h.numbers.resource(id).await.unwrap().e164
    }

    async fn resolve(
        &self,
        peer: &cf_service_auth::ServiceTokenSigner,
        family: &str,
        e164: &str,
    ) -> Reply {
        // Part IV M: SMS has its own resolver (number-sms-route.resolve).
        let (path, body) = if family == "SMS" {
            (RESOLVE_SMS, json!({ "destination": e164 }))
        } else {
            (RESOLVE, json!({ "family": family, "e164": e164 }))
        };
        self.h.internal_json(peer, Method::POST, path, body).await
    }

    async fn verify(
        &self,
        peer: &cf_service_auth::ServiceTokenSigner,
        grant: &str,
        consume: bool,
        operation: Option<&str>,
    ) -> Reply {
        let mut body = json!({ "grant": grant, "consume": consume });
        if let Some(o) = operation {
            body["operationRef"] = json!(o);
        }
        self.h.internal_json(peer, Method::POST, VERIFY, body).await
    }

    async fn caller_identity(
        &self,
        s: &OwnerScope,
        principal: Option<Uuid>,
        number: Uuid,
        operation: &str,
    ) -> Reply {
        self.h
            .internal_delegated(
                &self.h.peers.calling,
                CALLER,
                json!({ "numberResourceId": number, "operationRef": operation }),
                delegated(s, principal),
            )
            .await
    }
}

fn grant_of(r: &Reply) -> String {
    r.body["grant"]["grant"]
        .as_str()
        .or_else(|| r.body["grant"].as_str())
        .unwrap_or_else(|| panic!("no grant: {:?}", r.body))
        .to_string()
}

fn sandbox() -> OwnerScope {
    scope(EnvironmentKind::Sandbox)
}

// ---------------------------------------------------------------------------------------
// Steps 15–16: inbound voice / SMS resolution
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_inbound_call_resolves_from_canonical_state_with_a_grant_for_calling() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let queue = Uuid::new_v4();
    let inbox = Uuid::new_v4();
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Department,
            None,
            targets(
                (V::EnterpriseQueue, Some(queue)),
                (Sms::Department, Some(inbox)),
            ),
        )
        .await;
    let e164 = t.e164(id).await;
    let r = t.resolve(&t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{GRANT}InboundResolution"), &r.body);
    assert_eq!(r.body["outcome"], "ROUTED");
    assert_eq!(r.body["assignmentId"], a.id.to_string());
    assert_eq!(r.body["scope"]["tenantId"], s.tenant_id.to_string());
    assert_eq!(
        r.body["target"],
        json!({ "kind": "ENTERPRISE_QUEUE", "ref": queue })
    );
    assert_eq!(r.body["grant"]["audience"], "cfmeet.calling");
    assert_eq!(r.body["grant"]["grantKind"], "INBOUND_VOICE");
    // No telephone number travels in the response or the grant (Calling is its audience).
    let grant = grant_of(&r);
    assert!(!r.body.to_string().contains(&e164[1..]));
    // The audience (Calling) verifies it; nobody else can use it.
    let v = t.verify(&t.h.peers.calling, &grant, false, None).await;
    assert_eq!(v.body["valid"], true, "{:?}", v.body);
    conforms(&format!("{GRANT}GrantVerification"), &v.body);
    assert_eq!(v.body["target"]["ref"], queue.to_string());
    assert_eq!(v.body["e164"], Value::Null);
    let v = t.verify(&t.h.peers.telephony, &grant, false, None).await;
    assert_eq!(
        (v.body["valid"].as_bool(), v.body["reason"].as_str()),
        (Some(false), Some("AUDIENCE_MISMATCH"))
    );
    // An inbound grant is never consumed.
    let v = t.verify(&t.h.peers.calling, &grant, true, None).await;
    assert_eq!(v.status, StatusCode::BAD_REQUEST);
    // SMS: resolved by the SMS Gateway, addressed to it.
    let r = t.resolve(&t.h.peers.sms_gateway, "SMS", &e164).await;
    assert_eq!(r.body["outcome"], "ROUTED", "{:?}", r.body);
    assert_eq!(
        r.body["target"],
        json!({ "kind": "DEPARTMENT", "ref": inbox })
    );
    let v = t
        .verify(&t.h.peers.sms_gateway, &grant_of(&r), false, None)
        .await;
    assert_eq!(v.body["valid"], true);
    // Only the carrier of a family resolves it, each through its OWN resolver and
    // permission; Calling resolves nothing.
    let r = t.resolve(&t.h.peers.telephony, "SMS", &e164).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    let r = t.resolve(&t.h.peers.sms_gateway, "VOICE", &e164).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    // The voice resolver never serves SMS, even for its own caller.
    let r =
        t.h.internal_json(
            &t.h.peers.telephony,
            Method::POST,
            RESOLVE,
            json!({ "family": "SMS", "e164": e164 }),
        )
        .await;
    assert_eq!(
        r.body["error"]["reason"], "FAMILY_NOT_SUPPORTED",
        "{:?}",
        r.body
    );
    let r = t.resolve(&t.h.peers.calling, "VOICE", &e164).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    // Every grant is recorded (never the token itself).
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_routing_grants")
            .await,
        2
    );
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM information_schema.columns WHERE table_name = 'number_routing_grants' AND column_name IN ('token', 'grant', 'jws', 'signature')").await,
        0
    );
}

#[tokio::test]
async fn unknown_unassigned_unready_and_unrouted_numbers_never_get_a_grant() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let outcome = |r: &Reply| {
        (
            r.body["outcome"].as_str().unwrap().to_string(),
            r.body["reason"].as_str().map(str::to_string),
        )
    };
    let r = t
        .resolve(&t.h.peers.telephony, "VOICE", "+15550009999")
        .await;
    assert_eq!(
        outcome(&r),
        ("UNAVAILABLE".into(), Some("UNKNOWN_NUMBER".into()))
    );
    let r = t
        .resolve(&t.h.peers.telephony, "VOICE", "not a number")
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Discovered but never assigned: not ACTIVE.
    let id = t.id_of(&s, "syn-voice-only").await;
    let r = t
        .resolve(&t.h.peers.telephony, "VOICE", &t.e164(id).await)
        .await;
    assert_eq!(
        outcome(&r),
        ("UNAVAILABLE".into(), Some("NOT_ACTIVE".into()))
    );
    // An offboarded employee line is ACTIVE and ready but routes nowhere.
    let e1 = t.dir.add(P::Active);
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(e1)), (Sms::Principal, Some(e1))),
        )
        .await;
    t.a.offboard_employee(&ctx(), &s, a.id, 1, None)
        .await
        .unwrap();
    let e164 = t.e164(id).await;
    let r = t.resolve(&t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(outcome(&r), ("NOT_ROUTED".into(), None));
    assert_eq!(r.body["grant"], Value::Null);
    conforms(&format!("{GRANT}InboundResolution"), &r.body);
    // A revoked inbound capability: unavailable at once.
    t.h.numbers
        .revoke_capability(
            &ctx(),
            Actor::operator("ops.k"),
            id,
            C::SmsInbound,
            "OPERATOR_REVOKED",
        )
        .await
        .unwrap();
    let r = t.resolve(&t.h.peers.sms_gateway, "SMS", &e164).await;
    assert_eq!(
        outcome(&r),
        ("UNAVAILABLE".into(), Some("CAPABILITY_NOT_USABLE".into()))
    );
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_routing_grants")
            .await,
        0
    );
}

// ---------------------------------------------------------------------------------------
// Steps 17–18: caller identity and SMS sender grants
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_caller_identity_grant_is_single_use_and_only_for_the_scopes_own_number() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets((V::EnterpriseIvr, Some(Uuid::new_v4())), (Sms::None, None)),
        )
        .await;
    let operation = op();
    let r = t.caller_identity(&s, None, id, &operation).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{GRANT}IssuedGrant"), &r.body);
    assert_eq!(
        (r.body["audience"].as_str(), r.body["grantKind"].as_str()),
        (Some("cfmeet.telephony"), Some("CALLER_IDENTITY"))
    );
    // Calling holds the grant but never learns the number from it.
    let grant = grant_of(&r);
    let e164 = t.e164(id).await;
    let payload = grant.split('.').nth(1).unwrap();
    let claims = String::from_utf8(
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload).unwrap(),
    )
    .unwrap();
    assert!(!claims.contains(&e164[1..]));
    conforms(
        &format!("{GRANT}RoutingGrantClaims"),
        &serde_json::from_str(&claims).unwrap(),
    );
    // Telephony (the audience) verifies, consumes once and only then learns the number.
    let v = t.verify(&t.h.peers.telephony, &grant, false, None).await;
    assert_eq!(
        (v.body["valid"].as_bool(), v.body["consumed"].as_bool()),
        (Some(true), Some(false))
    );
    let v = t
        .verify(
            &t.h.peers.telephony,
            &grant,
            true,
            Some("call-other-operation"),
        )
        .await;
    assert_eq!(v.body["reason"], "OPERATION_MISMATCH");
    let v = t
        .verify(&t.h.peers.telephony, &grant, true, Some(&operation))
        .await;
    assert_eq!(v.body["valid"], true, "{:?}", v.body);
    assert_eq!(
        (v.body["e164"].as_str(), v.body["consumed"].as_bool()),
        (Some(e164.as_str()), Some(true))
    );
    assert_eq!(v.body["assignmentId"], a.id.to_string());
    let v = t
        .verify(&t.h.peers.telephony, &grant, true, Some(&operation))
        .await;
    assert_eq!(v.body["reason"], "GRANT_ALREADY_USED");
    let v = t.verify(&t.h.peers.calling, &grant, false, None).await;
    assert_eq!(v.body["reason"], "AUDIENCE_MISMATCH");
    // One grant per operation.
    let r = t.caller_identity(&s, None, id, &operation).await;
    assert_eq!(r.body["error"]["reason"], "OPERATION_ALREADY_AUTHORIZED");
    // Spoofing: another scope's number is indistinguishable from a missing one, a
    // number without outbound voice is refused, nothing is authorized without the
    // delegated context or the permission, and no arbitrary number is accepted.
    let other = sandbox();
    assert_eq!(
        t.caller_identity(&other, None, id, &op()).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.caller_identity(&s, None, Uuid::new_v4(), &op())
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let r =
        t.h.internal_json(
            &t.h.peers.calling,
            Method::POST,
            CALLER,
            json!({ "numberResourceId": id, "operationRef": op() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r =
        t.h.internal_delegated(
            &t.h.peers.telephony,
            CALLER,
            json!({ "numberResourceId": id, "operationRef": op() }),
            delegated(&s, None),
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let r =
        t.h.internal_delegated(
            &t.h.peers.calling,
            CALLER,
            json!({ "e164": e164, "operationRef": op() }),
            delegated(&s, None),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        t.caller_identity(&s, None, id, "short").await.status,
        StatusCode::BAD_REQUEST
    );
    // The inbound-only number has no outbound voice.
    let (inbound_only, _) = t
        .active(
            &s,
            "syn-inbound-only",
            NumberType::Geographic,
            &[C::VoiceInbound, C::SmsInbound],
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let r = t.caller_identity(&s, None, inbound_only, &op()).await;
    assert_eq!(r.body["error"]["reason"], "CAPABILITY_NOT_USABLE");
}

#[tokio::test]
async fn personal_and_employee_numbers_are_presented_only_by_their_person() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let alice = t.dir.add(P::Active);
    let bob = t.dir.add(P::Active);
    let (personal, _) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Personal,
            Some(alice),
            targets((V::Principal, Some(alice)), (Sms::Principal, Some(alice))),
        )
        .await;
    for who in [None, Some(bob)] {
        let r = t.caller_identity(&s, who, personal, &op()).await;
        assert_eq!(
            r.body["error"]["reason"], "PRINCIPAL_NOT_PERMITTED",
            "{who:?}"
        );
    }
    assert_eq!(
        t.caller_identity(&s, Some(alice), personal, &op())
            .await
            .status,
        StatusCode::OK
    );
    let (line, a) = t
        .active(
            &s,
            "syn-voice-only",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound],
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(bob)), (Sms::None, None)),
        )
        .await;
    assert_eq!(
        t.caller_identity(&s, Some(alice), line, &op()).await.body["error"]["reason"],
        "PRINCIPAL_NOT_PERMITTED"
    );
    let ok = t.caller_identity(&s, Some(bob), line, &op()).await;
    assert_eq!(ok.status, StatusCode::OK);
    // Bob leaves: his outstanding grant dies with the routing change and he can no
    // longer obtain one.
    t.a.offboard_employee(&ctx(), &s, a.id, 1, None)
        .await
        .unwrap();
    let v = t
        .verify(&t.h.peers.telephony, &grant_of(&ok), false, None)
        .await;
    assert_eq!(v.body["reason"], "GRANT_REVOKED");
    assert_eq!(
        t.caller_identity(&s, Some(bob), line, &op()).await.body["error"]["reason"],
        "PRINCIPAL_NOT_PERMITTED"
    );
}

/// Part IV L: Calling's view of a number before asking for a caller-identity grant —
/// the delegated scope's own ACTIVE assignment and voice target (no number, no provider
/// reference); another scope's number is NOT_FOUND; only Calling, only delegated.
#[tokio::test]
async fn calling_reads_the_scopes_current_assignment_and_voice_target_only() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let bob = t.dir.add(P::Active);
    let (line, a) = t
        .active(
            &s,
            "syn-voice-only",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound],
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(bob)), (Sms::None, None)),
        )
        .await;
    let path = format!("/internal/v1/routing/number-assignments/{line}");
    let r =
        t.h.internal_get_delegated(&t.h.peers.calling, &path, delegated(&s, Some(bob)))
            .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["assignmentId"], json!(a.id));
    assert_eq!(r.body["ownerKind"], "ORGANIZATION");
    assert_eq!(r.body["voice"]["kind"], "PRINCIPAL");
    assert_eq!(r.body["voice"]["ref"], json!(bob));
    assert_eq!(r.body["outbound"]["voice"], true);
    assert_eq!(r.body["outbound"]["sms"], false);
    assert_eq!(r.body["numberActive"], true);
    let e164 = t.e164(line).await;
    assert!(
        !r.body.to_string().contains(&e164[1..]),
        "no number in the view"
    );
    conforms(&format!("{GRANT}CurrentAssignment"), &r.body);
    // Another scope's view of the same number: NOT_FOUND (no existence oracle).
    let other = scope(EnvironmentKind::Test);
    let r =
        t.h.internal_get_delegated(&t.h.peers.calling, &path, delegated(&other, Some(bob)))
            .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    // Without a delegated context, or as another service: refused.
    let r = t.h.internal(&t.h.peers.calling, Method::GET, &path).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    let r =
        t.h.internal_get_delegated(&t.h.peers.telephony, &path, delegated(&s, Some(bob)))
            .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
}

/// Part IV O: the SMS Gateway re-checks a sender right before submission with its own
/// sender permission (number-sms-sender.authorize) — the delegated scope's view only.
#[tokio::test]
async fn the_sms_gateway_reads_the_scopes_current_sender_assignment_only() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-mobile",
            NumberType::Mobile,
            &[C::SmsInbound, C::SmsOutbound],
            O::SharedTeam,
            None,
            targets((V::None, None), (Sms::SharedTeam, Some(Uuid::new_v4()))),
        )
        .await;
    let path = format!("/internal/v1/routing/number-assignments/{id}");
    let r =
        t.h.internal_get_delegated(&t.h.peers.sms_gateway, &path, delegated(&s, None))
            .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["assignmentId"], json!(a.id));
    assert_eq!(r.body["assignmentStatus"], "ACTIVE");
    assert_eq!(r.body["outbound"]["sms"], true);
    conforms(&format!("{GRANT}CurrentAssignment"), &r.body);
    let other = scope(EnvironmentKind::Test);
    let r =
        t.h.internal_get_delegated(&t.h.peers.sms_gateway, &path, delegated(&other, None))
            .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let r =
        t.h.internal(&t.h.peers.sms_gateway, Method::GET, &path)
            .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
}

#[tokio::test]
async fn an_sms_sender_grant_needs_outbound_sms_and_is_consumed_by_the_gateway() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let caps = [C::SmsInbound, C::SmsOutbound];
    let (id, _) = t
        .active(
            &s,
            "syn-voice-sms-mobile",
            NumberType::Mobile,
            &caps,
            O::SharedTeam,
            None,
            targets((V::None, None), (Sms::SharedTeam, Some(Uuid::new_v4()))),
        )
        .await;
    let operation = format!("msg-{}", Uuid::new_v4().simple());
    let r =
        t.h.internal_delegated(
            &t.h.peers.sms_gateway,
            SENDER,
            json!({ "numberResourceId": id, "operationRef": operation }),
            delegated(&s, None),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(
        (r.body["audience"].as_str(), r.body["grantKind"].as_str()),
        (Some("cfmeet.sms-gateway"), Some("SMS_SENDER"))
    );
    let v = t
        .verify(
            &t.h.peers.sms_gateway,
            &grant_of(&r),
            true,
            Some(&operation),
        )
        .await;
    assert_eq!(
        (v.body["valid"].as_bool(), v.body["e164"].as_str()),
        (Some(true), Some(t.e164(id).await.as_str()))
    );
    // Calling is not an SMS sender; a voice-only number has no outbound SMS.
    let r =
        t.h.internal_delegated(
            &t.h.peers.calling,
            SENDER,
            json!({ "numberResourceId": id, "operationRef": op() }),
            delegated(&s, None),
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let (geo, _) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let r =
        t.h.internal_delegated(
            &t.h.peers.sms_gateway,
            SENDER,
            json!({ "numberResourceId": geo, "operationRef": op() }),
            delegated(&s, None),
        )
        .await;
    assert_eq!(r.body["error"]["reason"], "CAPABILITY_NOT_USABLE");
}

// ---------------------------------------------------------------------------------------
// Step 19: revocation, expiry and forgery
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn routing_changes_suspension_transfer_and_capability_revocation_revoke_grants_at_once() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let e164 = t.e164(id).await;
    let inbound = grant_of(&t.resolve(&t.h.peers.telephony, "VOICE", &e164).await);
    let outbound = grant_of(&t.caller_identity(&s, None, id, &op()).await);
    // A routing change: every outstanding grant of the number is revoked, recorded.
    t.a.update_routing(
        &ctx(),
        &s,
        a.id,
        1,
        targets(
            (V::EnterpriseRingGroup, Some(Uuid::new_v4())),
            (Sms::None, None),
        ),
        "ROUTING_CHANGED",
    )
    .await
    .unwrap();
    assert_eq!(
        t.verify(&t.h.peers.calling, &inbound, false, None)
            .await
            .body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        t.verify(&t.h.peers.telephony, &outbound, false, None)
            .await
            .body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        strings(
            t.h.conn(),
            "SELECT DISTINCT revoke_reason FROM number_routing_grants"
        )
        .await,
        vec!["ROUTING_CHANGED".to_string()]
    );
    // A fresh resolution names the new revision.
    let r = t.resolve(&t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(r.body["target"]["kind"], "ENTERPRISE_RING_GROUP");
    let fresh = grant_of(&r);
    assert_eq!(
        t.verify(&t.h.peers.calling, &fresh, false, None).await.body["valid"],
        true
    );
    // A capability revocation: the grant stops verifying (the capability is re-checked).
    let out2 = grant_of(&t.caller_identity(&s, None, id, &op()).await);
    t.h.numbers
        .revoke_capability(
            &ctx(),
            Actor::operator("ops.k"),
            id,
            C::VoiceOutbound,
            "OPERATOR_REVOKED",
        )
        .await
        .unwrap();
    assert_eq!(
        t.verify(&t.h.peers.telephony, &out2, false, None)
            .await
            .body["reason"],
        "CAPABILITY_NOT_USABLE"
    );
    // Suspension: the assignment leaves ACTIVE; grants are revoked, nothing resolves.
    let current = t.h.numbers.assignment(&s, a.id).await.unwrap();
    t.h.numbers
        .set_assignment_suspended(
            &ctx(),
            customer_actor(&s),
            a.id,
            current.revision,
            true,
            "CUSTOMER_SUSPENDED",
        )
        .await
        .unwrap();
    assert_eq!(
        t.verify(&t.h.peers.calling, &fresh, false, None).await.body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        t.resolve(&t.h.peers.telephony, "VOICE", &e164).await.body["outcome"],
        "UNAVAILABLE"
    );
    assert!(
        t.h.scalar("SELECT count(*) FROM number_routing_grants WHERE revoke_reason = 'ASSIGNMENT_NOT_ACTIVE'").await >= 1
    );
}

#[tokio::test]
async fn a_transfer_revokes_the_predecessors_grants_and_the_successor_is_resolved() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let e1 = t.dir.add(P::Active);
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(e1)), (Sms::Principal, Some(e1))),
        )
        .await;
    let e164 = t.e164(id).await;
    let before = grant_of(&t.resolve(&t.h.peers.telephony, "VOICE", &e164).await);
    let dept = Uuid::new_v4();
    let decision = t
        .decide(
            &s,
            (SubjectKind::Organization, s.tenant_id),
            O::Department,
            NumberType::Geographic,
            &GEO,
            id,
        )
        .await;
    let out =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            TransferRequest {
                ownership_class: O::Department,
                owner_principal_id: None,
                targets: targets(
                    (V::EnterpriseDepartment, Some(dept)),
                    (Sms::Department, Some(dept)),
                ),
                eligibility_decision_id: Some(decision),
                expected_routing_revision: 1,
            },
            &key(),
        )
        .await
        .unwrap();
    assert_eq!(
        t.verify(&t.h.peers.calling, &before, false, None)
            .await
            .body["reason"],
        "GRANT_REVOKED"
    );
    let r = t.resolve(&t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(r.body["assignmentId"], out.assignment.id.to_string());
    assert_eq!(r.body["ownershipClass"], "DEPARTMENT");
    assert_eq!(
        t.verify(&t.h.peers.calling, &grant_of(&r), false, None)
            .await
            .body["valid"],
        true
    );
}

#[tokio::test]
async fn expired_forged_tampered_and_foreign_tokens_never_verify() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, _) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let r = t.caller_identity(&s, None, id, &op()).await;
    let grant = grant_of(&r);
    // Short-lived: 60 s (+ the 30 s skew) and it is gone.
    t.clock.advance(91);
    assert_eq!(
        t.verify(&t.h.peers.telephony, &grant, false, None)
            .await
            .body["reason"],
        "GRANT_EXPIRED"
    );
    // A grant signed by any other key (even with Numbering's kid and identity).
    let forged = NumberGrantSigner::new(
        ServiceId::numbering(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("numbering-grant-test-1").unwrap()),
        60,
        Arc::new(t.clock.clone()),
    )
    .unwrap()
    .sign(&cf_service_auth::number_grant::NumberGrantRequest {
        kind: NumberGrantKind::CallerIdentity,
        audience: ServiceId::telephony(),
        number_resource_id: id,
        assignment_id: Uuid::new_v4(),
        routing_revision: 0,
        scope: cf_service_auth::number_grant::GrantScope {
            tenant_id: s.tenant_id,
            application_id: s.application_id,
            environment_id: s.environment_id,
        },
        principal_id: None,
        target: None,
        operation_ref: Some(op()),
    })
    .unwrap();
    assert_eq!(
        t.verify(
            &t.h.peers.telephony,
            forged.expose_for_delivery(),
            false,
            None
        )
        .await
        .body["reason"],
        "GRANT_INVALID"
    );
    // Tampered payload, garbage, an over-long value.
    let fresh = grant_of(&t.caller_identity(&s, None, id, &op()).await);
    let mut parts: Vec<String> = fresh.split('.').map(str::to_string).collect();
    parts[1] = parts[1].chars().rev().collect();
    assert_eq!(
        t.verify(&t.h.peers.telephony, &parts.join("."), false, None)
            .await
            .body["reason"],
        "GRANT_INVALID"
    );
    assert_eq!(
        t.verify(&t.h.peers.telephony, "not.a.grant", false, None)
            .await
            .body["reason"],
        "GRANT_INVALID"
    );
    assert_eq!(
        t.verify(&t.h.peers.telephony, &"a".repeat(5000), false, None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // A peer without number-grant.verify (Privacy Relay is not even trusted).
    let r =
        t.h.internal_json(
            &t.h.peers.privacy_relay,
            Method::POST,
            VERIFY,
            json!({ "grant": fresh }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn concurrent_consumption_performs_the_operation_exactly_once() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, _) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let operation = op();
    let grant = grant_of(&t.caller_identity(&s, None, id, &operation).await);
    let (a, b, c) = tokio::join!(
        t.verify(&t.h.peers.telephony, &grant, true, Some(&operation)),
        t.verify(&t.h.peers.telephony, &grant, true, Some(&operation)),
        t.verify(&t.h.peers.telephony, &grant, true, Some(&operation)),
    );
    let consumed = [a, b, c]
        .iter()
        .filter(|r| r.body["consumed"] == true)
        .count();
    assert_eq!(consumed, 1);
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_routing_grants WHERE consumed_at IS NOT NULL")
            .await,
        1
    );
}

// ---------------------------------------------------------------------------------------
// Production safety, configuration, database and contracts
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_protected_runtime_never_routes_synthetic_inventory() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, _) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    for runtime in [RuntimeEnvironment::Staging, RuntimeEnvironment::Production] {
        let clock: cf_service_auth::SharedClock = Arc::new(t.clock.clone());
        let grants = RoutingGrants::new(
            t.h.numbers.clone(),
            NumberGrantSigner::new(
                ServiceId::numbering(),
                runtime,
                ServiceSigningKey::generate(KeyId::parse("numbering-grant-prod-1").unwrap()),
                60,
                clock.clone(),
            )
            .unwrap(),
            clock,
        );
        let out = grants
            .resolve_inbound(
                &ctx(),
                &ServiceId::telephony(),
                RouteFamily::Voice,
                &t.e164(id).await,
            )
            .await
            .unwrap();
        assert!(
            matches!(out, InboundResolution::Unavailable { reason: r } if r == reason::SYNTHETIC_NOT_ROUTABLE)
        );
        let err = grants
            .authorize_outbound(
                &ctx(),
                &ServiceId::calling(),
                NumberGrantKind::CallerIdentity,
                DelegatedScope {
                    tenant_id: s.tenant_id,
                    application_id: s.application_id,
                    environment_id: s.environment_id,
                    principal_id: None,
                },
                id,
                &op(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "SYNTHETIC_NOT_ROUTABLE");
    }
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_routing_grants")
            .await,
        0
    );
}

#[tokio::test]
async fn without_a_grant_key_nothing_is_granted_or_verified() {
    let Some(h) = Harness::new().await else {
        return;
    };
    for (peer, path, body) in [
        (
            &h.peers.telephony,
            RESOLVE,
            json!({ "family": "VOICE", "e164": "+15550000008" }),
        ),
        (&h.peers.telephony, VERIFY, json!({ "grant": "a.b.c" })),
    ] {
        let r = h.internal_json(peer, Method::POST, path, body).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE")
        );
    }
    let r = h
        .internal_delegated(
            &h.peers.calling,
            CALLER,
            json!({ "numberResourceId": Uuid::new_v4(), "operationRef": op() }),
            delegated(&sandbox(), None),
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_database_guards_grants_and_the_migration_rolls_back_cleanly() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, _) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let operation = op();
    let grant = grant_of(&t.caller_identity(&s, None, id, &operation).await);
    t.verify(&t.h.peers.telephony, &grant, true, Some(&operation))
        .await;
    for (sql, expected) in [
        (
            "UPDATE number_routing_grants SET routing_revision = routing_revision + 1",
            "immutable",
        ),
        (
            "UPDATE number_routing_grants SET audience = 'cfmeet.calling'",
            "immutable",
        ),
        (
            "UPDATE number_routing_grants SET consumed_at = now(), consumed_by = 'cfmeet.calling'",
            "consumed once",
        ),
        ("DELETE FROM number_routing_grants", "kept until"),
        ("TRUNCATE number_routing_grants", "append-only"),
        (
            "UPDATE number_routing_grants SET revoked_at = now(), revoke_reason = NULL",
            "ck_number_routing_grants_revoked",
        ),
    ] {
        let err = exec(t.h.conn(), sql).await.unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains(&expected.to_lowercase()),
            "{sql}: {err}"
        );
    }
    // An inbound grant without a target, or a grant living longer than 300 s, is refused.
    let err = exec(
        t.h.conn(),
        "INSERT INTO number_routing_grants (id, token_id, grant_kind, number_resource_id, assignment_id, tenant_id, application_id, \
           environment_id, routing_revision, audience, requested_by, issued_at, expires_at, correlation_id) \
         SELECT gen_random_uuid(), md5(random()::text), 'INBOUND_VOICE', number_resource_id, assignment_id, tenant_id, application_id, \
           environment_id, 1, 'cfmeet.calling', 'cfmeet.telephony', now(), now() + interval '1 hour', 'corr' FROM number_routing_grants LIMIT 1",
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("ck_number_routing_grants"), "{err}");
    use numbering_migration::{Migrator, MigratorTrait};
    let steps = common::steps_down_to("m20261023_000009_routing_grants");
    let err = Migrator::down(t.h.conn(), Some(steps)).await.unwrap_err();
    assert!(err.to_string().contains("Workstream K"), "{err}");
    let Some(clean) = common::TestDb::new().await else {
        return;
    };
    Migrator::down(&clean.conn, Some(steps)).await.unwrap();
    assert_eq!(
        scalar(&clean.conn, "SELECT count(*) FROM information_schema.tables WHERE table_name = 'number_routing_grants'").await,
        0
    );
    Migrator::up(&clean.conn, None).await.unwrap();
}

#[tokio::test]
async fn the_grant_surface_is_internal_only_and_in_the_served_contract() {
    let Some(t) = setup().await else {
        return;
    };
    let served =
        t.h.raw(
            axum::http::Request::get("/openapi.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    for path in [
        "/internal/v1/routing/inbound-resolutions",
        "/internal/v1/routing/caller-identity-grants",
        "/internal/v1/routing/sms-sender-grants",
        "/internal/v1/routing/grant-verifications",
    ] {
        let ops = &served.body["paths"][path];
        assert!(ops.get("post").is_some(), "{path}");
        assert_eq!(
            ops["post"]["security"][0]
                .as_object()
                .unwrap()
                .keys()
                .next()
                .unwrap(),
            "cfServiceAuth"
        );
    }
    // No customer route mentions grants; the served document holds no key material.
    let text = served.body.to_string();
    for (path, _) in served.body["paths"].as_object().unwrap() {
        assert!(
            !(path.starts_with("/v1/") && path.contains("grant")),
            "{path}"
        );
    }
    assert!(!text.contains("PRIVATE KEY") && !text.contains("\"d\":"));
    // A customer credential never reaches the internal routes.
    let r =
        t.h.customer_json(
            "cfk_test_whatever",
            Method::POST,
            CALLER,
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let _ = (COMMON, t.h.conn());
}

/// Step 5 of the L closure: the routing-revision check on its own. Only the number's
/// routing revision moves — same ACTIVE assignment, owner, targets, readiness,
/// capabilities and expiry, and no revocation of outstanding grants — so a grant issued at
/// the previous revision is refused for exactly that reason (no other check can refuse
/// it), and a grant issued at the current revision verifies.
#[tokio::test]
async fn a_grant_from_a_previous_routing_revision_is_refused_as_stale() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let queue = Uuid::new_v4();
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound],
            O::Organization,
            None,
            targets((V::EnterpriseQueue, Some(queue)), (Sms::None, None)),
        )
        .await;
    let e164 = t.e164(id).await;
    let first = t.resolve(&t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(first.body["outcome"], "ROUTED", "{:?}", first.body);
    let old_revision = first.body["routingRevision"].as_i64().unwrap();
    let inbound = grant_of(&first);
    let outbound = grant_of(&t.caller_identity(&s, None, id, &op()).await);
    assert_eq!(
        t.verify(&t.h.peers.calling, &inbound, false, None)
            .await
            .body["valid"],
        true
    );

    // Only the number's routing revision moves (what a carrier route re-provisioning
    // records on the canonical number): same ACTIVE assignment, owner, targets, readiness,
    // capabilities and expiry, and nothing revokes the outstanding grants.
    common::exec(
        t.h.conn(),
        &format!("UPDATE number_resources SET routing_revision = routing_revision + 1, revision = revision + 1 WHERE id = '{id}'"),
    )
    .await
    .unwrap();
    let resource = t.h.numbers.resource(id).await.unwrap();
    assert!(resource.routing_revision > old_revision);
    let still = t.h.numbers.assignment(&s, a.id).await.unwrap();
    assert_eq!(
        (
            still.status.as_str(),
            still.revision,
            still.routing_profile_revision
        ),
        ("ACTIVE", a.revision, a.routing_profile_revision),
        "the assignment and its routing profile are unchanged"
    );
    assert_eq!(
        t.h.scalar(&format!(
            "SELECT count(*) FROM number_routing_grants WHERE number_resource_id = '{id}' AND revoked_at IS NOT NULL"
        ))
        .await,
        0,
        "nothing revoked the outstanding grants: only the revision check can refuse them"
    );

    // The previous revision's grants are refused as stale (then recorded revoked).
    for (peer, grant, consume, operation) in [
        (&t.h.peers.calling, &inbound, false, None),
        (&t.h.peers.telephony, &outbound, true, Some(op())),
    ] {
        let v = t.verify(peer, grant, consume, operation.as_deref()).await;
        assert_eq!(v.body["valid"], false, "{:?}", v.body);
        assert_eq!(v.body["reason"], "STALE_ROUTING_REVISION", "{:?}", v.body);
    }

    // A grant issued at the current revision verifies.
    let fresh = t.resolve(&t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(fresh.body["outcome"], "ROUTED", "{:?}", fresh.body);
    assert_eq!(
        fresh.body["routingRevision"].as_i64(),
        Some(resource.routing_revision)
    );
    assert_eq!(fresh.body["inventoryClass"], "SYNTHETIC");
    let v = t
        .verify(&t.h.peers.calling, &grant_of(&fresh), false, None)
        .await;
    assert_eq!(v.body["valid"], true, "{:?}", v.body);
    assert_eq!(
        v.body["routingRevision"].as_i64(),
        Some(resource.routing_revision)
    );
    assert_eq!(v.body["inventoryClass"], "SYNTHETIC");
    assert_eq!(
        v.body["target"],
        json!({ "kind": "ENTERPRISE_QUEUE", "ref": queue })
    );
}
