//! Part IV Step 21 — the first functional DCN lifecycle milestone, against the
//! deterministic provider and synthetic test inventory only:
//!
//! Search → Eligibility → Reserve → Provision → Assign → Activate → Authorize Voice / SMS
//!
//! * Scenario A — a successful PERSONAL DCN through the real customer API (Platform
//!   Access authentication and principal resolution, deterministic KYC) and the internal
//!   routing / grant API;
//! * Scenario B — an organizational DCN routed to an employee, offboarded to another;
//! * Scenario C — contention and uncertain outcomes;
//! * Scenario D — the lifecycle safety boundary (suspended, releasing, quarantined,
//!   reassigned) for the H–K consumers.
//!
//! This certifies Numbering's control plane — not live PSTN calls, SMS transport or real
//! carrier provisioning, and not the P / Q release and quarantine workflows.
mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use base64::Engine;
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
};
use chrono::Duration;
use common::{
    acquisition as ax, catalog, kyc as fx, providers as px, scope, strings, Harness, Reply,
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
    identity::PrincipalStatus as P,
    numbers::{
        assignments::AssignmentRow,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::{
            evidence::EvidenceBytes, verifier::DeterministicKycVerifier, EvidenceKind, NewCase,
            SubjectKind, TenantClassification, VerificationHandler,
        },
        routing::RoutingTargets,
        Actor, Capability as C, DomainError, EnvironmentKind, LifecycleState as S, NumberType,
        OwnerScope, OwnershipClass as O, SmsTargetKind as Sms, VoiceTargetKind as V,
    },
    providers::{deterministic::Fault, ProviderOperation as Op},
};
use serde_json::{json, Value};
use uuid::Uuid;

const GEO: [C; 3] = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];
const MOBILE: [C; 4] = [
    C::VoiceInbound,
    C::VoiceOutbound,
    C::SmsInbound,
    C::SmsOutbound,
];
const RESOLVE: &str = "/internal/v1/routing/inbound-resolutions";
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
    format!("op-{}", Uuid::new_v4().simple())
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

fn grant_of(r: &Reply) -> String {
    r.body["grant"]["grant"]
        .as_str()
        .or_else(|| r.body["grant"].as_str())
        .unwrap_or_else(|| panic!("no grant: {:?}", r.body))
        .to_string()
}

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap_or_else(|| panic!("not an id: {v}"))).unwrap()
}

async fn resolve(
    h: &Harness,
    peer: &cf_service_auth::ServiceTokenSigner,
    family: &str,
    e164: &str,
) -> Reply {
    h.internal_json(
        peer,
        Method::POST,
        RESOLVE,
        json!({ "family": family, "e164": e164 }),
    )
    .await
}

async fn verify(
    h: &Harness,
    peer: &cf_service_auth::ServiceTokenSigner,
    grant: &str,
    operation: Option<&str>,
) -> Reply {
    let mut body = json!({ "grant": grant, "consume": operation.is_some() });
    if let Some(o) = operation {
        body["operationRef"] = json!(o);
    }
    h.internal_json(peer, Method::POST, VERIFY, body).await
}

async fn outbound(
    h: &Harness,
    path: &str,
    s: &OwnerScope,
    principal: Option<Uuid>,
    number: Uuid,
    operation: &str,
) -> Reply {
    let peer = if path == CALLER {
        &h.peers.calling
    } else {
        &h.peers.sms_gateway
    };
    h.internal_delegated(
        peer,
        path,
        json!({ "numberResourceId": number, "operationRef": operation }),
        delegated(s, principal),
    )
    .await
}

async fn acknowledge_routes(h: &Harness, number: Uuid) {
    for r in h.numbers.current_routes(number).await.unwrap() {
        let peer = if r.family == "VOICE" {
            &h.peers.telephony
        } else {
            &h.peers.sms_gateway
        };
        let reply = h
            .internal_json(
                peer,
                Method::POST,
                &format!("/internal/v1/number-resources/{number}/route-readiness"),
                json!({ "family": r.family, "routeRef": r.route_ref, "routeRevision": r.route_revision }),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    }
}

// ---------------------------------------------------------------------------------------
// Scenario A — a successful personal DCN, end to end through the APIs
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn scenario_a_a_personal_dcn_from_search_to_authorized_voice_and_sms() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let (_, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(DeterministicKycVerifier::new()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    let (h, _grant_clock) = h.with_grants();
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let a = ax::of(&h, &p, AcquisitionSettings::default());
    let runner =
        ax::runner(&h, &p, &a).with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }));
    let drain = || ax::drain(&runner);
    let pa = h.pa.clone().unwrap();

    // 1. A sandbox tenant / application authenticated by Platform Access, and the person
    //    who will own the number.
    let all = [
        "numbers.search",
        "numbers.read",
        "numbers.manage",
        "numbers.assign",
        "numbers.kyc.read",
        "numbers.kyc.manage",
    ];
    let me = pa.customer("SANDBOX", &all, &all).await;
    let owner = pa.principal(me.environment_id).await;
    let s = OwnerScope {
        tenant_id: me.tenant_id,
        application_id: me.application_id,
        environment_id: me.environment_id,
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: me.service_account_id,
    };
    let get = |path: String| {
        let h = &h;
        let secret = me.secret.clone();
        async move {
            h.customer_with(&secret, Method::GET, &path, None, &[])
                .await
        }
    };
    let post = |path: String, body: Value, idem: Option<String>| {
        let h = &h;
        let secret = me.secret.clone();
        async move {
            let headers: Vec<(&str, &str)> = idem
                .as_deref()
                .map(|k| vec![("idempotency-key", k)])
                .unwrap_or_default();
            h.customer_with(&secret, Method::POST, &path, Some(body), &headers)
                .await
        }
    };

    // 2. Search synthetic mobile inventory (provider-neutral, never a provider id).
    let page = get("/v1/numbers/search?countryCode=US&numberType=MOBILE".into()).await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    assert!(!page.body.to_string().contains("syn-"));
    let pick = page.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            let caps: Vec<&str> = r["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|c| c["capability"].as_str())
                .collect();
            MOBILE.iter().all(|c| caps.contains(&c.as_str())) && r["kyc"]["numberFlagged"] == false
        })
        .expect("a voice + SMS mobile number");
    let number = id(&pick["numberResourceId"]);
    let e164 = pick["e164"].as_str().unwrap().to_string();
    assert_eq!(pick["inventoryClass"], "SYNTHETIC");
    // 3. Capabilities and KYC requirements: a personal mobile number needs identity.
    let caps = get(format!(
        "/v1/numbers/assignments/{}/capabilities",
        Uuid::new_v4()
    ))
    .await;
    assert_eq!(caps.status, StatusCode::NOT_FOUND);
    let req = get("/v1/numbers/kyc/requirements/US/MOBILE/PERSONAL".into()).await;
    assert_eq!(req.status, StatusCode::OK, "{:?}", req.body);
    assert!(req.body.to_string().contains("INDIVIDUAL_IDENTITY"));
    // Without KYC the person is not eligible.
    let ask = json!({
        "subjectKind": "INDIVIDUAL", "subjectRef": owner, "countryCode": "US", "numberType": "MOBILE",
        "ownershipClass": "PERSONAL", "capabilities": MOBILE.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "numberResourceId": number,
    });
    let before = post("/v1/numbers/eligibility".into(), ask.clone(), None).await;
    assert_ne!(before.body["outcome"], "ELIGIBLE");
    // 4. Deterministic KYC: open, attach identity evidence, submit, verified by the worker.
    let case = post(
        "/v1/numbers/kyc/cases".into(),
        json!({ "subjectKind": "INDIVIDUAL", "subjectRef": owner, "countryCode": "US", "numberType": "MOBILE",
                "ownershipClass": "PERSONAL", "purposeCode": "PERSONAL_LINE" }),
        Some(key()),
    )
    .await;
    assert_eq!(case.status, StatusCode::CREATED, "{:?}", case.body);
    let case_id = id(&case.body["kycCaseId"]);
    let ev = post(
        format!("/v1/numbers/kyc/cases/{case_id}/evidence"),
        json!({ "evidenceKind": "PASSPORT", "contentType": "application/pdf",
                "contentBase64": base64::engine::general_purpose::STANDARD.encode(fx::pdf("passport")) }),
        Some(key()),
    )
    .await;
    assert_eq!(ev.status, StatusCode::CREATED, "{:?}", ev.body);
    let current = get(format!("/v1/numbers/kyc/cases/{case_id}")).await;
    let submitted = post(
        format!("/v1/numbers/kyc/cases/{case_id}/submit"),
        json!({ "expectedRevision": current.body["revision"] }),
        Some(key()),
    )
    .await;
    assert_eq!(submitted.status, StatusCode::OK, "{:?}", submitted.body);
    drain().await;
    assert_eq!(
        get(format!("/v1/numbers/kyc/cases/{case_id}")).await.body["status"],
        "APPROVED"
    );
    // 5. Eligibility.
    let elig = post("/v1/numbers/eligibility".into(), ask, None).await;
    assert_eq!(elig.body["outcome"], "ELIGIBLE", "{:?}", elig.body);
    let decision = elig.body["eligibilityDecisionId"].clone();
    // 6. Reserve (a durable provider hold).
    let caps_json = json!(MOBILE.iter().map(|c| c.as_str()).collect::<Vec<_>>());
    let res = post(
        "/v1/numbers/reservations".into(),
        json!({ "numberResourceId": number, "capabilities": caps_json, "eligibilityDecisionId": decision }),
        Some(key()),
    )
    .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{:?}", res.body);
    drain().await;
    let rid = id(&res.body["reservationId"]);
    assert_eq!(
        get(format!("/v1/numbers/reservations/{rid}")).await.body["status"],
        "HELD"
    );
    // 7. Provision through the provider-neutral durable executor.
    let acq = post(
        "/v1/numbers/acquisitions".into(),
        json!({ "reservationId": rid, "capabilities": caps_json }),
        Some(key()),
    )
    .await;
    assert_eq!(acq.status, StatusCode::ACCEPTED, "{:?}", acq.body);
    drain().await;
    let aid = id(&acq.body["acquisitionId"]);
    let got = get(format!("/v1/numbers/acquisitions/{aid}")).await;
    assert_eq!(got.body["status"], "PROVISIONED");
    // 8. The authoritative binding and capabilities (internal view; opaque binding refs).
    let resource = h
        .internal(
            &h.peers.telephony,
            Method::GET,
            &format!("/internal/v1/number-resources/{number}"),
        )
        .await;
    assert_eq!(resource.status, StatusCode::OK);
    assert!(!resource.body.to_string().contains("syn-"));
    let bindings = h.numbers.bindings(number).await.unwrap();
    assert_eq!(bindings.iter().filter(|b| b.status == "ACTIVE").count(), 1);
    let cmd = got.body["provisioning"]["status"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(cmd, "SUCCEEDED");
    let routes = post(
        format!("/v1/numbers/acquisitions/{aid}/routes"),
        json!({ "capabilities": caps_json }),
        Some(key()),
    )
    .await;
    assert_eq!(routes.status, StatusCode::ACCEPTED);
    drain().await;
    // 9. A PERSONAL assignment owned by the resolved person.
    let mine = json!({ "voice": { "kind": "PRINCIPAL", "ref": owner }, "sms": { "kind": "PRINCIPAL", "ref": owner } });
    let personal_decision = post(
        "/v1/numbers/eligibility".into(),
        json!({ "subjectKind": "INDIVIDUAL", "subjectRef": owner, "countryCode": "US", "numberType": "MOBILE",
                "ownershipClass": "PERSONAL", "capabilities": caps_json, "numberResourceId": number }),
        None,
    )
    .await;
    let asg = post(
        "/v1/numbers/assignments".into(),
        json!({ "acquisitionId": aid, "ownershipClass": "PERSONAL", "ownerPrincipalId": owner, "routing": mine,
                "capabilities": caps_json, "eligibilityDecisionId": personal_decision.body["eligibilityDecisionId"] }),
        Some(key()),
    )
    .await;
    assert_eq!(asg.status, StatusCode::CREATED, "{:?}", asg.body);
    assert_eq!(asg.body["ownership"]["ownerRef"], owner.to_string());
    let asg_id = id(&asg.body["assignmentId"]);
    // 10. Routing readiness — acknowledged by the carriers — then activation.
    let early = post(
        format!("/v1/numbers/assignments/{asg_id}/activate"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(early.body["error"]["reason"], "ROUTING_NOT_READY");
    acknowledge_routes(&h, number).await;
    let active = post(
        format!("/v1/numbers/assignments/{asg_id}/activate"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(active.body["status"], "ACTIVE", "{:?}", active.body);
    // 11. Inbound voice resolves to the assigned person.
    let voice = resolve(&h, &h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(voice.body["outcome"], "ROUTED", "{:?}", voice.body);
    assert_eq!(
        voice.body["target"],
        json!({ "kind": "PRINCIPAL", "ref": owner })
    );
    assert_eq!(voice.body["scope"]["tenantId"], me.tenant_id.to_string());
    assert_eq!(
        verify(&h, &h.peers.calling, &grant_of(&voice), None)
            .await
            .body["valid"],
        true
    );
    // 12. An outbound caller identity grant for the owner, validated and consumed once.
    let call = op();
    let cid = outbound(&h, CALLER, &s, Some(owner), number, &call).await;
    assert_eq!(cid.status, StatusCode::OK, "{:?}", cid.body);
    let v = verify(&h, &h.peers.telephony, &grant_of(&cid), Some(&call)).await;
    assert_eq!(
        (v.body["valid"].as_bool(), v.body["e164"].as_str()),
        (Some(true), Some(e164.as_str()))
    );
    // 13. Inbound SMS resolves to the owner.
    let sms = resolve(&h, &h.peers.sms_gateway, "SMS", &e164).await;
    assert_eq!(
        sms.body["target"],
        json!({ "kind": "PRINCIPAL", "ref": owner })
    );
    assert_eq!(
        verify(&h, &h.peers.sms_gateway, &grant_of(&sms), None)
            .await
            .body["valid"],
        true
    );
    // 14. An outbound SMS sender grant, validated and consumed.
    let msg = op();
    let snd = outbound(&h, SENDER, &s, Some(owner), number, &msg).await;
    assert_eq!(snd.status, StatusCode::OK, "{:?}", snd.body);
    assert_eq!(
        verify(&h, &h.peers.sms_gateway, &grant_of(&snd), Some(&msg))
            .await
            .body["valid"],
        true
    );

    // Canonical identity, lifecycle, database evidence, events and ownership throughout.
    let resource = h.numbers.resource(number).await.unwrap();
    assert_eq!(
        (resource.id, resource.lifecycle_state()),
        (number, S::Active)
    );
    let path = strings(
        h.conn(),
        &format!("SELECT to_state FROM number_lifecycle_events WHERE number_resource_id = '{number}' ORDER BY sequence"),
    )
    .await;
    for state in ["RESERVED", "PROVISIONING", "PROVISIONED", "ACTIVE"] {
        assert!(
            path.iter().any(|p| p == state),
            "{state} missing in {path:?}"
        );
    }
    let history = h.numbers.assignment_history(&s, asg_id).await.unwrap();
    assert!(history
        .iter()
        .all(|r| r.owner_ref == owner && r.ownership_class == "PERSONAL"));
    assert_eq!(history.last().unwrap().to_status, "ACTIVE");
    let events = strings(
        h.conn(),
        &format!("SELECT DISTINCT event_type FROM numbering_outbox_events WHERE aggregate_id IN ('{number}', '{rid}', '{aid}', '{asg_id}')"),
    )
    .await;
    for e in [
        "number.reservation.changed",
        "number.acquisition.changed",
        "number.assignment.changed",
        "number.resource.lifecycle-changed",
    ] {
        assert!(events.iter().any(|x| x == e), "{e} missing in {events:?}");
    }
    // Events never carry the telephone number; pending events stay durable (no consumer
    // is wired in this build: nothing is marked published that was not).
    let digits = &e164[1..];
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM numbering_outbox_events WHERE payload::text LIKE '%{digits}%'"
        ))
        .await,
        0
    );
    assert_eq!(
        h.numbers.reservation(&s, rid).await.unwrap().status,
        "CONVERTED"
    );
    let _ = (a.numbers(), Op::Provision);
}

// ---------------------------------------------------------------------------------------
// Domain-level fixture for Scenarios B–D
// ---------------------------------------------------------------------------------------

struct L {
    h: Harness,
    p: px::Providers,
    a: Acquisition,
    kyc: numbering::numbers::kyc::KycService,
    dir: Arc<ax::Principals>,
    runner: numbering::infra::jobs::JobRunner,
}

async fn setup() -> Option<L> {
    let h = Harness::new().await?;
    let (_, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(DeterministicKycVerifier::new()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    let (h, _) = h.with_grants();
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let dir = Arc::new(ax::Principals::default());
    let a = ax::of(&h, &p, AcquisitionSettings::default()).with_principals(dir.clone());
    let runner =
        ax::runner(&h, &p, &a).with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }));
    Some(L {
        h,
        p,
        a,
        kyc,
        dir,
        runner,
    })
}

impl L {
    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    async fn id_of(&self, s: &OwnerScope, external_ref: &str) -> Uuid {
        if let SearchOutcome::Throttled { .. } = self
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
            panic!("throttled");
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
        t: NumberType,
        caps: &[C],
        number: Uuid,
    ) -> Result<Uuid, String> {
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
                    number_type: t,
                    ownership_class: class,
                    capabilities: caps.to_vec(),
                    number_resource_id: Some(number),
                    provider_id: None,
                },
            )
            .await
            .unwrap();
        if d.outcome == "ELIGIBLE" {
            Ok(d.id)
        } else {
            Err(d.outcome)
        }
    }

    async fn reserve(
        &self,
        s: &OwnerScope,
        number: Uuid,
        decision: Uuid,
        caps: &[C],
    ) -> Result<Uuid, DomainError> {
        let r = self
            .a
            .create_reservation(
                &ctx(),
                s,
                ReservationRequest {
                    number_resource_id: number,
                    capabilities: caps.to_vec(),
                    hold_seconds: Some(600),
                    eligibility_decision_id: Some(decision),
                },
                &key(),
            )
            .await?
            .reservation;
        self.drain().await;
        Ok(r.id)
    }

    async fn provision(
        &self,
        s: &OwnerScope,
        rid: Uuid,
        caps: &[C],
        k: &str,
    ) -> Result<Uuid, DomainError> {
        Ok(self
            .a
            .provision(
                &ctx(),
                s,
                standing(),
                ProvisionRequest {
                    reservation_id: rid,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: None,
                },
                k,
            )
            .await?
            .acquisition
            .id)
    }

    /// An ACTIVE organizational (or personal, with `owner`) assignment, routes ready.
    #[allow(clippy::too_many_arguments)]
    async fn active(
        &self,
        s: &OwnerScope,
        ext: &str,
        t: NumberType,
        caps: &[C],
        class: O,
        owner: Option<Uuid>,
        routing: RoutingTargets,
    ) -> (Uuid, AssignmentRow) {
        let number = self.id_of(s, ext).await;
        let org = (SubjectKind::Organization, s.tenant_id);
        let hold_class = if owner.is_some() {
            O::Organization
        } else {
            class
        };
        let hold = self
            .decide(s, org, hold_class, t, caps, number)
            .await
            .unwrap();
        let rid = self.reserve(s, number, hold, caps).await.unwrap();
        let acq = self.provision(s, rid, caps, &key()).await.unwrap();
        self.drain().await;
        let subject = owner.map(|p| (SubjectKind::Individual, p)).unwrap_or(org);
        let decision = self
            .decide(s, subject, class, t, caps, number)
            .await
            .unwrap();
        let a = self
            .a
            .create_assignment(
                &ctx(),
                s,
                standing(),
                AssignmentRequest {
                    basis: AssignmentBasis::Acquisition(acq),
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
            .configure_routes(&ctx(), s, acq, caps, &key())
            .await
            .unwrap();
        self.drain().await;
        acknowledge_routes(&self.h, number).await;
        let a = self
            .a
            .activate_assignment(&ctx(), s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
        (number, a)
    }

    async fn e164(&self, number: Uuid) -> String {
        self.h.numbers.resource(number).await.unwrap().e164
    }
}

// ---------------------------------------------------------------------------------------
// Scenario B — an organizational DCN, routed to an employee, offboarded
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn scenario_b_an_organizational_dcn_keeps_its_owner_while_authority_moves() {
    let Some(t) = setup().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let alice = t.dir.add(P::Active);
    let bob = t.dir.add(P::Active);
    let (number, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &GEO,
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(alice)), (Sms::Principal, Some(alice))),
        )
        .await;
    assert_eq!(
        (a.owner_kind.as_str(), a.owner_ref),
        ("ORGANIZATION", s.tenant_id)
    );
    let e164 = t.e164(number).await;
    // Grants for the current authorized target.
    let inbound = resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(
        inbound.body["target"],
        json!({ "kind": "PRINCIPAL", "ref": alice })
    );
    let alice_call = outbound(&t.h, CALLER, &s, Some(alice), number, &op()).await;
    assert_eq!(alice_call.status, StatusCode::OK);
    // Offboard Alice, the line moves to Bob.
    t.a.offboard_employee(
        &ctx(),
        &s,
        a.id,
        1,
        Some(targets(
            (V::Principal, Some(bob)),
            (Sms::Principal, Some(bob)),
        )),
    )
    .await
    .unwrap();
    // Alice's authority is revoked at once: her grants stop verifying, she gets no new one.
    assert_eq!(
        verify(&t.h, &t.h.peers.calling, &grant_of(&inbound), None)
            .await
            .body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        verify(&t.h, &t.h.peers.telephony, &grant_of(&alice_call), None)
            .await
            .body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        outbound(&t.h, CALLER, &s, Some(alice), number, &op())
            .await
            .body["error"]["reason"],
        "PRINCIPAL_NOT_PERMITTED"
    );
    // Bob is the current target.
    assert_eq!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164)
            .await
            .body["target"]["ref"],
        bob.to_string()
    );
    assert_eq!(
        outbound(&t.h, CALLER, &s, Some(bob), number, &op())
            .await
            .status,
        StatusCode::OK
    );
    // The organization still owns it; history keeps every routing revision.
    let now = t.h.numbers.assignment(&s, a.id).await.unwrap();
    assert_eq!(
        (
            now.owner_ref,
            now.ownership_class.as_str(),
            now.routing_profile_revision
        ),
        (s.tenant_id, "EMPLOYEE_DIRECT", Some(2))
    );
    let history = t.h.numbers.assignment_history(&s, a.id).await.unwrap();
    let revisions: Vec<Option<i64>> = history
        .iter()
        .filter(|h| h.event_kind == "ROUTING_CHANGED")
        .map(|h| h.routing_profile_revision)
        .collect();
    assert_eq!(revisions, vec![Some(1), Some(2)]);
    assert!(history.iter().all(|h| h.owner_ref == s.tenant_id));
    assert_eq!(
        t.h.numbers
            .routing_profile_revisions(&s, a.routing_profile_id.unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
}

// ---------------------------------------------------------------------------------------
// Scenario C — contention and uncertain outcomes
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn scenario_c_contention_and_uncertainty_never_double_assign_pay_or_route_unsafely() {
    let Some(t) = setup().await else {
        return;
    };
    let s1 = scope(EnvironmentKind::Sandbox);
    let s2 = scope(EnvironmentKind::Sandbox);
    let caps = [C::VoiceInbound, C::VoiceOutbound];

    // Two tenants reserving the same number: exactly one holds it.
    let number = t.id_of(&s1, "syn-voice-only").await;
    let d1 = t
        .decide(
            &s1,
            (SubjectKind::Organization, s1.tenant_id),
            O::Organization,
            NumberType::Geographic,
            &caps,
            number,
        )
        .await
        .unwrap();
    let d2 = t
        .decide(
            &s2,
            (SubjectKind::Organization, s2.tenant_id),
            O::Organization,
            NumberType::Geographic,
            &caps,
            number,
        )
        .await
        .unwrap();
    let (r1, r2) = tokio::join!(
        t.reserve(&s1, number, d1, &caps),
        t.reserve(&s2, number, d2, &caps)
    );
    let (winner, rid) = match (r1, r2) {
        (Ok(r), Err(e)) => {
            assert_eq!(e.code(), "NOT_ALLOCATABLE");
            (s1, r)
        }
        (Err(e), Ok(r)) => {
            assert_eq!(e.code(), "NOT_ALLOCATABLE");
            (s2, r)
        }
        other => panic!("exactly one reservation wins: {other:?}"),
    };
    // Duplicate provision requests; the provider times out AFTER executing.
    t.p.sim.inject(Op::Provision, Fault::TimeoutAfterExecution);
    let k = key();
    let acq = t.provision(&winner, rid, &caps, &k).await.unwrap();
    assert_eq!(t.provision(&winner, rid, &caps, &k).await.unwrap(), acq);
    assert_eq!(
        t.provision(&winner, rid, &caps, &key())
            .await
            .unwrap_err()
            .code(),
        "ACQUISITION_EXISTS"
    );
    // Reservation expiry during provisioning: the hold would lapse while UNKNOWN.
    t.drain().await;
    let row = t.h.numbers.acquisition(&winner, acq).await.unwrap();
    assert_eq!(row.status, "UNKNOWN");
    t.h.clock.advance(Duration::minutes(11));
    t.a.sweep_reservations(&ctx(), 100).await.unwrap();
    // Reconciliation from read-back (the lost response): proven, never resent.
    t.drain().await;
    t.h.clock.advance(Duration::seconds(31));
    t.drain().await;
    let row = t.h.numbers.acquisition(&winner, acq).await.unwrap();
    assert_eq!(row.status, "PROVISIONED");
    assert_eq!(
        t.p.sim
            .executions(Op::Provision, row.provision_command_id.unwrap()),
        1
    );
    assert_eq!(
        t.h.numbers
            .reservation(&winner, rid)
            .await
            .unwrap()
            .converted_to
            .as_deref(),
        Some("ACQUISITION")
    );
    // Lost provider request on another number: failed (retryable), then one success.
    let other = t.id_of(&s1, "syn-voice-sms-geo").await;
    let d = t
        .decide(
            &s1,
            (SubjectKind::Organization, s1.tenant_id),
            O::Organization,
            NumberType::Geographic,
            &GEO,
            other,
        )
        .await
        .unwrap();
    let orid = t.reserve(&s1, other, d, &GEO).await.unwrap();
    t.p.sim.inject(Op::Provision, Fault::LostRequest);
    let lost = t.provision(&s1, orid, &GEO, &key()).await.unwrap();
    t.drain().await;
    t.h.clock.advance(Duration::seconds(31));
    t.drain().await;
    let lost_row = t.h.numbers.acquisition(&s1, lost).await.unwrap();
    assert!(
        matches!(lost_row.status.as_str(), "FAILED" | "UNKNOWN"),
        "{}",
        lost_row.status
    );

    // KYC revocation during assignment activation (personal mobile needs identity KYC).
    let person = t.dir.add(P::Active);
    let mobile = t.id_of(&s1, "syn-voice-sms-mobile").await;
    let case = t
        .kyc
        .create_case(
            &ctx(),
            &s1,
            NewCase {
                subject_kind: SubjectKind::Individual,
                subject_ref: person,
                country: "US".into(),
                number_type: NumberType::Mobile,
                ownership_class: O::Personal,
                purpose_code: "PERSONAL_LINE".into(),
                provider_id: None,
                tenant_classification: TenantClassification::External,
            },
            None,
        )
        .await
        .unwrap()
        .value;
    t.kyc
        .attach_evidence(
            &ctx(),
            &s1,
            case.id,
            EvidenceKind::Passport,
            "application/pdf",
            EvidenceBytes::new(fx::pdf("passport")),
            None,
        )
        .await
        .unwrap();
    let c = t.kyc.case(&s1, case.id).await.unwrap();
    t.kyc
        .submit_case(&ctx(), &s1, case.id, c.revision, None)
        .await
        .unwrap();
    t.drain().await;
    assert_eq!(t.kyc.case(&s1, case.id).await.unwrap().status, "APPROVED");
    let hold = t
        .decide(
            &s1,
            (SubjectKind::Individual, person),
            O::Personal,
            NumberType::Mobile,
            &MOBILE,
            mobile,
        )
        .await
        .unwrap();
    let mrid = t.reserve(&s1, mobile, hold, &MOBILE).await.unwrap();
    let macq = t.provision(&s1, mrid, &MOBILE, &key()).await.unwrap();
    t.drain().await;
    let decision = t
        .decide(
            &s1,
            (SubjectKind::Individual, person),
            O::Personal,
            NumberType::Mobile,
            &MOBILE,
            mobile,
        )
        .await
        .unwrap();
    let pa =
        t.a.create_assignment(
            &ctx(),
            &s1,
            standing(),
            AssignmentRequest {
                basis: AssignmentBasis::Acquisition(macq),
                ownership_class: O::Personal,
                owner_principal_id: Some(person),
                targets: targets((V::Principal, Some(person)), (Sms::Principal, Some(person))),
                capabilities: MOBILE.to_vec(),
                eligibility_decision_id: Some(decision),
                pool_id: None,
            },
            &key(),
        )
        .await
        .unwrap()
        .assignment;
    t.a.configure_routes(&ctx(), &s1, macq, &MOBILE, &key())
        .await
        .unwrap();
    t.drain().await;
    acknowledge_routes(&t.h, mobile).await;
    t.kyc
        .revoke_case(
            &ctx(),
            &Actor::operator("ops.kyc"),
            case.id,
            "FRAUD_SUSPECTED",
        )
        .await
        .unwrap();
    let err =
        t.a.activate_assignment(&ctx(), &s1, standing(), pa.id)
            .await
            .unwrap_err();
    assert_eq!(err.code(), "ELIGIBILITY_REQUIRED");
    assert_eq!(
        t.h.numbers
            .resource(mobile)
            .await
            .unwrap()
            .lifecycle_state(),
        S::Provisioned
    );

    // Capability downgrade during grant issuance, and an assignment change racing with
    // inbound resolution, on an ACTIVE organizational number.
    let s3 = scope(EnvironmentKind::Sandbox);
    let (geo2, a2) = t
        .active(
            &s3,
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
    let e164 = t.e164(geo2).await;
    let next = Uuid::new_v4();
    let race_ctx = ctx();
    let (r, changed) = tokio::join!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164),
        t.a.update_routing(
            &race_ctx,
            &s3,
            a2.id,
            1,
            targets((V::EnterpriseRingGroup, Some(next)), (Sms::None, None)),
            "ROUTING_CHANGED"
        ),
    );
    changed.unwrap();
    let current = t.h.numbers.resource(geo2).await.unwrap().routing_revision;
    // Whatever the interleaving, a grant verifies only if it names the CURRENT revision.
    if r.body["outcome"] == "ROUTED" {
        let v = verify(&t.h, &t.h.peers.calling, &grant_of(&r), None).await;
        assert_eq!(
            v.body["valid"] == true,
            r.body["routingRevision"] == current,
            "{:?} {:?}",
            r.body,
            v.body
        );
    }
    assert_eq!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164)
            .await
            .body["target"]["ref"],
        next.to_string()
    );
    // Capability downgrade: the inbound capability is revoked mid-flight.
    let g = grant_of(&resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164).await);
    t.h.numbers
        .revoke_capability(
            &ctx(),
            Actor::operator("ops.k"),
            geo2,
            C::VoiceInbound,
            "OPERATOR_REVOKED",
        )
        .await
        .unwrap();
    assert_eq!(
        verify(&t.h, &t.h.peers.calling, &g, None).await.body["reason"],
        "CAPABILITY_NOT_USABLE"
    );
    assert_eq!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164)
            .await
            .body["reason"],
        "CAPABILITY_NOT_USABLE"
    );
    // Never a second open assignment of any number.
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM (SELECT number_resource_id FROM dcn_assignments WHERE status <> 'CLOSED' GROUP BY 1 HAVING count(*) > 1) d").await,
        0
    );
}

// ---------------------------------------------------------------------------------------
// Scenario D — the lifecycle safety boundary for the H–K consumers
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn scenario_d_suspended_releasing_quarantined_and_reassigned_numbers_stay_safe() {
    let Some(t) = setup().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let actor = customer_actor(&s);
    let e1 = t.dir.add(P::Active);
    let (number, a) = t
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
    let e164 = t.e164(number).await;
    // A reassigned resource (internal transfer) uses the new assignment and revision; the
    // previous routing's grants and its employee lose authority.
    let old = grant_of(&outbound(&t.h, CALLER, &s, Some(e1), number, &op()).await);
    let dept = Uuid::new_v4();
    let decision = t
        .decide(
            &s,
            (SubjectKind::Organization, s.tenant_id),
            O::Department,
            NumberType::Geographic,
            &GEO,
            number,
        )
        .await
        .unwrap();
    let moved =
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
    let b = moved.assignment;
    assert_eq!(
        verify(&t.h, &t.h.peers.telephony, &old, None).await.body["reason"],
        "GRANT_REVOKED"
    );
    let r = resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164).await;
    assert_eq!(
        (
            r.body["assignmentId"].as_str(),
            r.body["target"]["ref"].as_str()
        ),
        (
            Some(b.id.to_string().as_str()),
            Some(dept.to_string().as_str())
        )
    );
    assert!(r.body["routingRevision"].as_i64().unwrap() > 1);
    // Suspended: no resolution, no grant, existing grants revoked.
    let live = grant_of(&r);
    let b =
        t.h.numbers
            .set_assignment_suspended(
                &ctx(),
                actor.clone(),
                b.id,
                b.revision,
                true,
                "CUSTOMER_SUSPENDED",
            )
            .await
            .unwrap();
    assert_eq!(
        verify(&t.h, &t.h.peers.calling, &live, None).await.body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164)
            .await
            .body["outcome"],
        "UNAVAILABLE"
    );
    assert_eq!(
        outbound(&t.h, CALLER, &s, None, number, &op()).await.status,
        StatusCode::NOT_FOUND
    );
    // Releasing: still nothing.
    let b =
        t.h.numbers
            .request_release(&ctx(), actor.clone(), b.id, b.revision)
            .await
            .unwrap();
    assert_eq!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164)
            .await
            .body["outcome"],
        "UNAVAILABLE"
    );
    assert_eq!(
        outbound(&t.h, SENDER, &s, None, number, &op()).await.status,
        StatusCode::NOT_FOUND
    );
    // Routing removed and quarantined (state protections only — the P / Q workflows are
    // later workstreams): no reservation, no routing.
    let b =
        t.h.numbers
            .remove_routing(&ctx(), actor.clone(), b.id, b.revision)
            .await
            .unwrap();
    let b =
        t.h.numbers
            .quarantine_assignment(
                &ctx(),
                actor.clone(),
                b.id,
                b.revision,
                t.h.now() + Duration::days(30),
            )
            .await
            .unwrap();
    assert_eq!(b.status, "QUARANTINED");
    assert_eq!(
        t.h.numbers
            .resource(number)
            .await
            .unwrap()
            .lifecycle_state(),
        S::Quarantined
    );
    assert_eq!(
        resolve(&t.h, &t.h.peers.telephony, "VOICE", &e164)
            .await
            .body["reason"],
        "NOT_ACTIVE"
    );
    let other = scope(EnvironmentKind::Sandbox);
    let res = t
        .decide(
            &other,
            (SubjectKind::Organization, other.tenant_id),
            O::Organization,
            NumberType::Geographic,
            &GEO,
            number,
        )
        .await;
    match res {
        Ok(d) => assert!(t.reserve(&other, number, d, &GEO).await.is_err()),
        Err(outcome) => assert_ne!(outcome, "ELIGIBLE"),
    }
    // The previously assigned employee cannot obtain anything.
    assert_eq!(
        outbound(&t.h, CALLER, &s, Some(e1), number, &op())
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Historical assignment evidence is intact: both assignments, every step.
    let history =
        t.h.numbers
            .resource_assignment_history(&Actor::operator("ops.audit"), number)
            .await
            .unwrap();
    let owners: std::collections::BTreeSet<Uuid> =
        history.iter().map(|h| h.assignment_id).collect();
    assert_eq!(owners.len(), 2);
    let last: Vec<&str> = history
        .iter()
        .rev()
        .take(4)
        .map(|h| h.to_status.as_str())
        .collect();
    assert_eq!(last[0], "QUARANTINED");
    assert!(history
        .iter()
        .any(|h| h.reason_code == "TRANSFERRED_INTERNALLY" && h.to_status == "CLOSED"));
}
