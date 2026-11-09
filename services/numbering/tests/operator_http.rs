//! INV-002: the Numbering operator (`/v1/admin/**`) and customer stewardship / KYC routes
//! that the operation-coverage baseline still listed as untested, exercised through the
//! real router with credentials authenticated by a REAL Platform Access:
//!
//! * number-resource administration: suspend, restore, release, quarantine, quarantine
//!   review, lifecycle evidence, usage, risk decisions;
//! * risk decisions: customer read and appeal, operator appeal decision;
//! * reassignment; porting (read, evidence, submit, cancel);
//! * relay-number retirement; number-type catalog withdrawal;
//! * KYC: operator review decisions and customer appeals.
//!
//! Each route: no credential (401), a credential without the scope / an external tenant
//! on an operator route (403), no named operator (400 OPERATOR_REQUIRED), malformed input
//! (400), another scope's or a missing record (404), a refused state change (409 / 422) and
//! the success path with its effect.
mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::{
    acquisition as ax, catalog, kyc as fx, platform_access::Customer, providers as px, Harness,
    Reply,
};
use numbering::{
    acquisition::{
        assignments::{AssignmentBasis, AssignmentRequest},
        provisioning::ProvisionRequest,
        reservations::ReservationRequest,
        search::{SearchOutcome, SearchRequest},
        Acquisition, AcquisitionSettings,
    },
    domain::{RequestContext, SharedClock},
    infra::{jobs::JobRunner, RetryPolicy},
    numbers::{
        assignments::AssignmentRow,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::{
            evidence::EvidenceBytes,
            verifier::{DeterministicKycVerifier, ManualKycVerifier},
            EvidenceKind, KycService, KycStatus, NewCase, SubjectKind, TenantClassification,
            VerificationHandler,
        },
        routing::RoutingTargets,
        Capability as C, EnvironmentKind, NumberType, OwnerScope, OwnershipClass as O,
        SmsTargetKind as Sms, VoiceTargetKind as V,
    },
    stewardship::{
        porting::{DeterministicPorting, PortOutcome},
        risk::{NewSignal, SignalKind},
        Stewardship, StewardshipSettings,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

const MOBILE: [C; 3] = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];
const ALL: [&str; 6] = [
    "numbers.search",
    "numbers.read",
    "numbers.manage",
    "numbers.assign",
    "numbers.kyc.read",
    "numbers.kyc.manage",
];
const OPERATOR: &str = "ops.inv002";

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
fn org_targets(team: Uuid) -> RoutingTargets {
    RoutingTargets {
        voice: (V::SharedTeam, Some(team)),
        sms: (Sms::SharedTeam, Some(team)),
    }
}
fn owner(c: &Customer) -> OwnerScope {
    OwnerScope {
        tenant_id: c.tenant_id,
        application_id: c.application_id,
        environment_id: c.environment_id,
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: c.service_account_id,
    }
}

/// The customers of one world: `alice` and `bob` hold every customer scope, `reader` only
/// `numbers.read`, `admin` is a FIRST_PARTY application with `numbers.admin`.
struct People {
    alice: Customer,
    bob: Customer,
    reader: Customer,
    admin: Customer,
}

struct W {
    h: Harness,
    a: Acquisition,
    st: Stewardship,
    runner: JobRunner,
    grant_clock: cf_service_auth::ManualClock,
    porting: Arc<DeterministicPorting>,
    kyc: KycService,
    p: People,
}

async fn world(settings: StewardshipSettings) -> Option<W> {
    let h = Harness::with_platform_access().await?;
    let (_, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(DeterministicKycVerifier::new()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    let providers = px::setup(&h);
    let h = h.with_providers(providers.commands.clone());
    let (mut h, grant_clock) = h.with_grants();
    let porting = Arc::new(DeterministicPorting::new(&["US"]));
    h.state = h
        .state
        .clone()
        .with_stewardship_settings(settings.clone())
        .with_porting(porting.clone());
    h.app = numbering::http::app(h.state.clone());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let a = ax::of(
        &h,
        &providers,
        AcquisitionSettings {
            risk: settings.risk.clone(),
            ..AcquisitionSettings::default()
        },
    );
    let runner = ax::runner(&h, &providers, &a)
        .with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }));
    let st = Stewardship::new(
        h.numbers.clone(),
        Some(providers.commands.clone()),
        Arc::new(EligibilityPolicy::default()),
        settings,
    )
    .with_porting(porting.clone());
    let pa = h.pa.clone().unwrap();
    let p = People {
        alice: pa.customer("SANDBOX", &ALL, &ALL).await,
        bob: pa.customer("SANDBOX", &ALL, &ALL).await,
        reader: pa
            .customer("SANDBOX", &["numbers.read"], &["numbers.read"])
            .await,
        admin: pa
            .customer_as(
                "SANDBOX",
                "FIRST_PARTY",
                &["numbers.admin"],
                &["numbers.admin"],
            )
            .await,
    };
    Some(W {
        h,
        a,
        st,
        runner,
        grant_clock,
        porting,
        kyc,
        p,
    })
}

impl W {
    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    fn advance(&self, by: Duration) {
        self.h.clock.advance(by);
        self.grant_clock.advance(by.num_seconds());
    }

    /// A request with `token` (none: unauthenticated), an optional operator and extra
    /// headers.
    async fn call(
        &self,
        token: Option<&Customer>,
        method: Method,
        path: &str,
        body: Option<Value>,
        operator: bool,
        idempotency_key: Option<&str>,
    ) -> Reply {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(c) = token {
            builder = builder.header("authorization", format!("Bearer {}", c.secret));
        }
        if operator {
            builder = builder.header("x-cf-operator", OPERATOR);
        }
        if let Some(k) = idempotency_key {
            builder = builder.header("idempotency-key", k);
        }
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        self.h
            .raw(builder.body(axum::body::Body::from(body)).unwrap())
            .await
    }

    async fn admin(&self, method: Method, path: &str, body: Option<Value>) -> Reply {
        self.call(Some(&self.p.admin), method, path, body, true, None)
            .await
    }

    /// 401 without a credential, 403 for an external tenant's credential (even with every
    /// customer scope), 400 OPERATOR_REQUIRED for the admin without a named operator.
    async fn admin_only(&self, method: Method, path: &str, body: Option<Value>) {
        let r = self
            .call(None, method.clone(), path, body.clone(), true, None)
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}: {:?}", r.body);
        assert_eq!(r.code(), "AUTHENTICATION_FAILED");
        let r = self
            .call(
                Some(&self.p.alice),
                method.clone(),
                path,
                body.clone(),
                true,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {:?}", r.body);
        assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
        let r = self
            .call(Some(&self.p.admin), method, path, body, false, None)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {:?}", r.body);
        assert_eq!(r.code(), "OPERATOR_REQUIRED");
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
            &common::strings(
                self.h.conn(),
                &format!("SELECT number_resource_id::text FROM number_provider_inventory_refs WHERE external_ref = '{external_ref}'"),
            )
            .await
            .pop()
            .unwrap(),
        )
        .unwrap()
    }

    async fn decide(&self, s: &OwnerScope, number: Uuid) -> Uuid {
        let d = self
            .h
            .numbers
            .evaluate_eligibility(
                &ctx(),
                s,
                standing(),
                &EligibilityPolicy::default(),
                EligibilityRequest {
                    subject_kind: SubjectKind::Organization,
                    subject_ref: s.tenant_id,
                    country: "US".into(),
                    number_type: NumberType::Geographic,
                    ownership_class: O::Organization,
                    capabilities: MOBILE.to_vec(),
                    number_resource_id: Some(number),
                    provider_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(d.outcome, "ELIGIBLE", "{d:?}");
        d.id
    }

    async fn acknowledge(&self, number: Uuid) {
        for r in self.h.numbers.current_routes(number).await.unwrap() {
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
                    &format!("/internal/v1/number-resources/{number}/route-readiness"),
                    json!({ "family": r.family, "routeRef": r.route_ref, "routeRevision": r.route_revision }),
                )
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
        }
    }

    /// An ACTIVE organizational mobile DCN (voice + SMS) of `s`, routes ready.
    async fn active(&self, s: &OwnerScope) -> (Uuid, AssignmentRow) {
        let number = self.id_of(s, "syn-voice-sms-geo").await;
        let hold = self.decide(s, number).await;
        let rid = self
            .a
            .create_reservation(
                &ctx(),
                s,
                ReservationRequest {
                    number_resource_id: number,
                    capabilities: MOBILE.to_vec(),
                    hold_seconds: Some(600),
                    eligibility_decision_id: Some(hold),
                },
                &key(),
            )
            .await
            .unwrap()
            .reservation
            .id;
        self.drain().await;
        let acq = self
            .a
            .provision(
                &ctx(),
                s,
                standing(),
                ProvisionRequest {
                    reservation_id: rid,
                    capabilities: MOBILE.to_vec(),
                    eligibility_decision_id: None,
                },
                &key(),
            )
            .await
            .unwrap()
            .acquisition
            .id;
        self.drain().await;
        let decision = self.decide(s, number).await;
        let a = self
            .a
            .create_assignment(
                &ctx(),
                s,
                standing(),
                AssignmentRequest {
                    basis: AssignmentBasis::Acquisition(acq),
                    ownership_class: O::Organization,
                    owner_principal_id: None,
                    targets: org_targets(Uuid::new_v4()),
                    capabilities: MOBILE.to_vec(),
                    eligibility_decision_id: Some(decision),
                    pool_id: None,
                },
                &key(),
            )
            .await
            .unwrap()
            .assignment;
        self.a
            .configure_routes(&ctx(), s, acq, &MOBILE, &key())
            .await
            .unwrap();
        self.drain().await;
        self.acknowledge(number).await;
        let a = self
            .a
            .activate_assignment(&ctx(), s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
        (number, a)
    }

    async fn settle_suspensions(&self) {
        self.drain().await;
        self.st.advance_suspensions(&ctx(), 10).await.unwrap();
    }
}

fn id(v: &Value, field: &str) -> String {
    v[field]
        .as_str()
        .unwrap_or_else(|| panic!("no {field}: {v}"))
        .to_string()
}

// ---------------------------------------------------------------------------------------
// Number-resource administration and reassignment
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn operators_administer_a_number_through_its_whole_lifecycle_and_another_tenant_reassigns_it()
{
    let Some(w) = world(StewardshipSettings::default()).await else {
        return;
    };
    let alice = owner(&w.p.alice);
    let (number, assignment) = w.active(&alice).await;
    let missing = Uuid::new_v4();
    let base = format!("/v1/admin/number-resources/{number}");
    let suspend_body = json!({ "reasonCode": "OPERATOR_HOLD" });
    let release_body = json!({ "reasonCode": "OPERATOR_RELEASE", "policy": "POOL_RETURN" });
    let justification = json!({ "purpose": "ACCOUNT_RECOVERY", "caseRef": "CASE-4001" });

    // Authentication, administration scope and the named operator, route by route.
    for (method, path, body) in [
        (
            Method::POST,
            format!("{base}/suspend"),
            Some(suspend_body.clone()),
        ),
        (Method::POST, format!("{base}/restore"), None),
        (
            Method::POST,
            format!("{base}/release"),
            Some(release_body.clone()),
        ),
        (Method::GET, format!("{base}/quarantine"), None),
        (
            Method::POST,
            format!("{base}/lifecycle-evidence"),
            Some(justification.clone()),
        ),
        (Method::GET, format!("{base}/usage"), None),
        (Method::GET, format!("{base}/risk-decisions"), None),
        (
            Method::POST,
            format!("/v1/admin/quarantines/{missing}/review"),
            Some(json!({ "decision": "APPROVE" })),
        ),
    ] {
        w.admin_only(method, &path, body).await;
    }

    // Malformed ids and bodies.
    for (method, path, body) in [
        (
            Method::POST,
            "/v1/admin/number-resources/not-a-number/suspend".to_string(),
            Some(suspend_body.clone()),
        ),
        (
            Method::GET,
            "/v1/admin/number-resources/42/quarantine".to_string(),
            None,
        ),
        (Method::POST, format!("{base}/suspend"), Some(json!({}))),
        (
            Method::POST,
            format!("{base}/suspend"),
            Some(json!({ "reasonCode": "OPERATOR_HOLD", "force": true })),
        ),
        (
            Method::POST,
            format!("{base}/lifecycle-evidence"),
            Some(json!({ "purpose": "CURIOSITY", "caseRef": "CASE-4001" })),
        ),
        (
            Method::POST,
            format!("{base}/lifecycle-evidence"),
            Some(json!({ "caseRef": "CASE-4001" })),
        ),
    ] {
        let r = w.admin(method, &path, body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {:?}", r.body);
    }
    // A number nobody holds: nothing to suspend, restore, release or show.
    let other = format!("/v1/admin/number-resources/{missing}");
    for (method, path, body) in [
        (
            Method::POST,
            format!("{other}/suspend"),
            Some(suspend_body.clone()),
        ),
        (Method::POST, format!("{other}/restore"), None),
        (Method::GET, format!("{other}/quarantine"), None),
        (
            Method::POST,
            format!("/v1/admin/quarantines/{missing}/review"),
            Some(json!({ "decision": "APPROVE" })),
        ),
    ] {
        let r = w.admin(method, &path, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {:?}", r.body);
        assert_eq!(r.code(), "NOT_FOUND");
    }
    let r = w
        .call(
            Some(&w.p.admin),
            Method::POST,
            &format!("{other}/release"),
            Some(release_body.clone()),
            true,
            Some("admin-release-missing"),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    // The ACTIVE number has no quarantine yet.
    let r = w
        .admin(Method::GET, &format!("{base}/quarantine"), None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);

    // Suspension by the operator (source OPERATOR), and restoration.
    let r = w
        .admin(Method::POST, &format!("{base}/suspend"), Some(suspend_body))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["source"], "OPERATOR");
    assert_eq!(r.body["assignmentId"], json!(assignment.id));
    assert_eq!(r.body["reasonCode"], "OPERATOR_HOLD");
    w.settle_suspensions().await;
    let r = w
        .admin(Method::POST, &format!("{base}/restore"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "RESTORING", "{:?}", r.body);
    w.settle_suspensions().await;
    let s = w
        .call(
            Some(&w.p.alice),
            Method::GET,
            &format!("/v1/numbers/assignments/{}/suspension", assignment.id),
            None,
            false,
            None,
        )
        .await;
    assert_eq!(s.body["status"], "RESTORED", "{:?}", s.body);

    // Usage with provider costs (operators only see costs).
    let usage = json!({
        "sourceKey": "inv002-leg-1", "numberResourceId": number, "assignmentId": assignment.id,
        "usageKind": "VOICE_OUTBOUND_SECONDS", "quantity": 61, "providerCostMinor": 12, "currency": "USD",
        "periodStart": w.h.now().to_rfc3339(), "periodEnd": w.h.now().to_rfc3339(),
    });
    let r =
        w.h.internal_json(
            &w.h.peers.telephony,
            Method::POST,
            "/internal/v1/number-usage",
            usage,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let r = w.admin(Method::GET, &format!("{base}/usage"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let rows = r.body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{:?}", r.body);
    assert_eq!(
        rows[0]["sourceKey"], "cfmeet.telephony:inv002-leg-1",
        "namespaced by reporter"
    );
    assert_eq!(rows[0]["assignmentId"], json!(assignment.id));
    assert_eq!(rows[0]["quantity"], 61);
    assert_eq!(
        rows[0]["providerCostMinor"], 12,
        "operators see provider costs"
    );
    assert_eq!(rows[0]["currency"], "USD");
    let r = w.admin(Method::GET, &format!("{other}/usage"), None).await;
    assert_eq!(r.body["data"], json!([]), "{:?}", r.body);

    // Risk decisions of the number (the allocation decision at least).
    let r = w
        .admin(Method::GET, &format!("{base}/risk-decisions"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    for d in r.body["data"].as_array().unwrap() {
        assert_eq!(d["numberResourceId"], json!(number), "{d}");
        assert!(d["riskDecisionId"].is_string());
    }
    let r = w
        .admin(Method::GET, &format!("{other}/risk-decisions"), None)
        .await;
    assert_eq!(r.body["data"], json!([]));

    // Release: the Idempotency-Key is mandatory; a replay answers 200.
    let release = format!("{base}/release");
    let r = w
        .admin(Method::POST, &release, Some(release_body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    let r = w
        .call(
            Some(&w.p.admin),
            Method::POST,
            &release,
            Some(json!({ "reasonCode": "OPERATOR_RELEASE", "policy": "SHRED" })),
            true,
            Some("admin-release-0001"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    let r = w
        .call(
            Some(&w.p.admin),
            Method::POST,
            &release,
            Some(release_body.clone()),
            true,
            Some("admin-release-0001"),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    assert_eq!(r.body["status"], "QUARANTINED", "{:?}", r.body);
    assert_eq!(r.body["policy"], "POOL_RETURN");
    let release_id = id(&r.body, "releaseId");
    let again = w
        .call(
            Some(&w.p.admin),
            Method::POST,
            &release,
            Some(release_body.clone()),
            true,
            Some("admin-release-0001"),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{:?}", again.body);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(id(&again.body, "releaseId"), release_id);
    // Released: the assignment is no longer ACTIVE, nothing is suspended.
    let r = w
        .admin(
            Method::POST,
            &format!("{base}/suspend"),
            Some(json!({ "reasonCode": "OPERATOR_HOLD" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "ASSIGNMENT_NOT_ACTIVE");

    // The quarantine, then its review: EXTEND, HOLD and APPROVE.
    let r = w
        .admin(Method::GET, &format!("{base}/quarantine"), None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["numberResourceId"], json!(number));
    assert_eq!(r.body["disposition"], "POOL_RETURN");
    assert_eq!(r.body["status"], "ACTIVE");
    let quarantine = id(&r.body, "quarantineId");
    let until = r.body["quarantineUntil"].clone();
    let review = format!("/v1/admin/quarantines/{quarantine}/review");
    for bad in [
        json!({ "decision": "EXTEND" }),
        json!({ "decision": "APPROVE", "days": 3 }),
        json!({ "decision": "FORGET" }),
        json!({ "decision": "EXTEND", "days": 0 }),
        json!({ "decision": "EXTEND", "days": 100000 }),
    ] {
        let r = w.admin(Method::POST, &review, Some(bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = w
        .admin(
            Method::POST,
            &review,
            Some(json!({ "decision": "EXTEND", "days": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["reviewDecision"], "EXTENDED");
    assert_eq!(r.body["extensions"], 1);
    assert_ne!(r.body["quarantineUntil"], until);
    let r = w
        .admin(Method::POST, &review, Some(json!({ "decision": "HOLD" })))
        .await;
    assert_eq!(r.body["status"], "HELD_FOR_REVIEW", "{:?}", r.body);
    assert_eq!(r.body["reviewRequired"], true);
    let r = w
        .admin(
            Method::POST,
            &review,
            Some(json!({ "decision": "APPROVE" })),
        )
        .await;
    assert_eq!(r.body["status"], "ACTIVE", "{:?}", r.body);
    assert_eq!(r.body["reviewDecision"], "APPROVED");
    assert_eq!(r.body["reviewRequired"], false);

    // Lifecycle evidence: audited, justified, never an owner reference.
    let before =
        w.h.scalar(&format!(
            "SELECT count(*) FROM number_history_access WHERE operator_ref = '{OPERATOR}'"
        ))
        .await;
    let r = w
        .admin(
            Method::POST,
            &format!("{base}/lifecycle-evidence"),
            Some(justification.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!(r.body["workflow"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["kind"] == "QUARANTINE"));
    assert_eq!(
        w.h.scalar(&format!(
            "SELECT count(*) FROM number_history_access WHERE operator_ref = '{OPERATOR}'"
        ))
        .await,
        before + 1
    );

    // After the cooldown the number is reassignable; a review of the completed
    // quarantine is refused.
    w.advance(Duration::days(9));
    assert_eq!(w.st.complete_quarantines(&ctx(), 10).await.unwrap(), 1);
    let r = w
        .admin(Method::POST, &review, Some(json!({ "decision": "HOLD" })))
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "QUARANTINE_COMPLETED");

    // Reassignment to bob over HTTP.
    let bob = owner(&w.p.bob);
    let decision = w.decide(&bob, number).await;
    let team = Uuid::new_v4();
    let body = json!({
        "numberResourceId": number, "ownershipClass": "ORGANIZATION",
        "routing": { "voice": { "kind": "SHARED_TEAM", "ref": team }, "sms": { "kind": "SHARED_TEAM", "ref": team } },
        "capabilities": ["VOICE_INBOUND", "VOICE_OUTBOUND", "SMS_INBOUND"],
        "eligibilityDecisionId": decision,
    });
    const REASSIGN: &str = "/v1/numbers/reassignments";
    let r = w
        .call(
            None,
            Method::POST,
            REASSIGN,
            Some(body.clone()),
            false,
            Some("reassign-0001"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(
            Some(&w.p.reader),
            Method::POST,
            REASSIGN,
            Some(body.clone()),
            false,
            Some("reassign-0001"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    let r = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            REASSIGN,
            Some(body.clone()),
            false,
            None,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "missing key: {:?}",
        r.body
    );
    let r = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            REASSIGN,
            Some(body.clone()),
            false,
            Some("short"),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "malformed key: {:?}",
        r.body
    );
    let mut bad = body.clone();
    bad["capabilities"] = json!(["TELEPORTATION"]);
    let r = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            REASSIGN,
            Some(bad),
            false,
            Some("reassign-0002"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    // Bob's eligibility decision is not alice's: alice cannot use it.
    let r = w
        .call(
            Some(&w.p.alice),
            Method::POST,
            REASSIGN,
            Some(body.clone()),
            false,
            Some("reassign-0003"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "ELIGIBILITY_REQUIRED");
    let r = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            REASSIGN,
            Some(body.clone()),
            false,
            Some("reassign-0004"),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let new_assignment = id(&r.body["assignment"], "assignmentId");
    assert_ne!(new_assignment, assignment.id.to_string());
    let replay = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            REASSIGN,
            Some(body),
            false,
            Some("reassign-0004"),
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{:?}", replay.body);
    assert_eq!(
        id(&replay.body["assignment"], "assignmentId"),
        new_assignment
    );
    // Alice no longer sees the number; bob does.
    let r = w
        .call(
            Some(&w.p.alice),
            Method::GET,
            &format!("/v1/numbers/assignments/{new_assignment}"),
            None,
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = w
        .call(
            Some(&w.p.bob),
            Method::GET,
            &format!("/v1/numbers/assignments/{new_assignment}"),
            None,
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
}

// ---------------------------------------------------------------------------------------
// Risk decisions and appeals
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_risk_decision_is_read_and_appealed_by_its_owner_and_decided_by_an_operator() {
    let Some(w) = world(StewardshipSettings::default()).await else {
        return;
    };
    let alice = owner(&w.p.alice);
    let (number, _) = w.active(&alice).await;
    let out =
        w.st.report_signal(
            &ctx(),
            "cfmeet.sms-gateway",
            NewSignal {
                number_resource_id: number,
                kind: SignalKind::SmsPumping,
                magnitude: 95,
                source_key: "inv002-pump-1".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(out.outcome, "RESTRICT");
    let decision = out.decision_id.unwrap();
    let read = format!("/v1/numbers/risk-decisions/{decision}");

    // Customer read.
    let r = w.call(None, Method::GET, &read, None, false, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(Some(&w.p.bob), Method::GET, &read, None, false, None)
        .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "another scope: {:?}",
        r.body
    );
    for (path, status) in [
        (
            "/v1/numbers/risk-decisions/latest".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("/v1/numbers/risk-decisions/{}", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let r = w
            .call(Some(&w.p.alice), Method::GET, &path, None, false, None)
            .await;
        assert_eq!(r.status, status, "{path}: {:?}", r.body);
    }
    let r = w
        .call(Some(&w.p.reader), Method::GET, &read, None, false, None)
        .await;
    // The reader holds numbers.read but belongs to another tenant: indistinguishable
    // from a missing decision.
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let r = w
        .call(Some(&w.p.alice), Method::GET, &read, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(id(&r.body, "riskDecisionId"), decision.to_string());
    assert_eq!(r.body["outcome"], "RESTRICT");
    assert_eq!(r.body["numberResourceId"], json!(number));
    let original = r.body.clone();

    // Customer appeal.
    let appeal = format!("{read}/appeals");
    let r = w
        .call(None, Method::POST, &appeal, Some(json!({})), false, None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(
            Some(&w.p.reader),
            Method::POST,
            &appeal,
            Some(json!({})),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    let r = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            &appeal,
            Some(json!({})),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    for bad in [
        json!({ "statementRef": "my-letter" }),
        json!({ "reason": "unfair" }),
    ] {
        let r = w
            .call(
                Some(&w.p.alice),
                Method::POST,
                &appeal,
                Some(bad.clone()),
                false,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = w
        .call(
            Some(&w.p.alice),
            Method::POST,
            &appeal,
            Some(json!({ "statementRef": Uuid::new_v4() })),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["outcome"], "PENDING");
    assert_eq!(r.body["supersedesId"], json!(decision));
    let appeal_id = id(&r.body, "riskDecisionId");
    // One open appeal per decision.
    let r = w
        .call(
            Some(&w.p.alice),
            Method::POST,
            &appeal,
            Some(json!({})),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "APPEAL_PENDING");

    // The operator's appeal decision.
    let decide = format!("/v1/admin/risk-decisions/{appeal_id}/appeal-decisions");
    w.admin_only(Method::POST, &decide, Some(json!({ "upheld": true })))
        .await;
    for bad in [
        json!({}),
        json!({ "upheld": "yes" }),
        json!({ "upheld": true, "note": "x" }),
    ] {
        let r = w.admin(Method::POST, &decide, Some(bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = w
        .admin(
            Method::POST,
            &format!(
                "/v1/admin/risk-decisions/{}/appeal-decisions",
                Uuid::new_v4()
            ),
            Some(json!({ "upheld": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let r = w
        .admin(Method::POST, &decide, Some(json!({ "upheld": true })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_ne!(r.body["outcome"], "PENDING", "{:?}", r.body);
    // Decided once.
    let r = w
        .admin(Method::POST, &decide, Some(json!({ "upheld": false })))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "APPEAL_DECIDED");
    // The original decision is never rewritten; the operator sees the whole chain.
    let r = w
        .call(Some(&w.p.alice), Method::GET, &read, None, false, None)
        .await;
    assert_eq!(r.body, original);
    let r = w
        .admin(
            Method::GET,
            &format!("/v1/admin/number-resources/{number}/risk-decisions"),
            None,
        )
        .await;
    let ids: Vec<String> = r.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| id(d, "riskDecisionId"))
        .collect();
    assert!(ids.contains(&decision.to_string()), "{ids:?}");
    assert!(ids.contains(&appeal_id), "{ids:?}");
}

// ---------------------------------------------------------------------------------------
// Porting
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn port_requests_are_read_evidenced_submitted_and_cancelled_by_their_owner_only() {
    let Some(w) = world(StewardshipSettings {
        porting_enabled: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let alice = owner(&w.p.alice);
    let (_, assignment) = w.active(&alice).await;
    let create = |body: Value, k: &'static str| {
        let w = &w;
        async move {
            w.call(
                Some(&w.p.alice),
                Method::POST,
                "/v1/numbers/ports",
                Some(body),
                false,
                Some(k),
            )
            .await
        }
    };
    let out = create(
        json!({ "direction": "OUT", "assignmentId": assignment.id, "ownershipClass": "ORGANIZATION" }),
        "inv002-port-out-1",
    )
    .await;
    assert_eq!(out.status, StatusCode::CREATED, "{:?}", out.body);
    assert_eq!(out.body["status"], "DOCUMENTS_REQUIRED");
    let port = id(&out.body, "portRequestId");
    let path = format!("/v1/numbers/ports/{port}");

    // Read.
    let r = w.call(None, Method::GET, &path, None, false, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(Some(&w.p.bob), Method::GET, &path, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    for (p, status) in [
        (
            "/v1/numbers/ports/port-1".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("/v1/numbers/ports/{}", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let r = w
            .call(Some(&w.p.alice), Method::GET, &p, None, false, None)
            .await;
        assert_eq!(r.status, status, "{p}");
    }
    let r = w
        .call(Some(&w.p.alice), Method::GET, &path, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body, out.body);

    // Evidence: KYC evidence of the owner's own scope only.
    let case = w
        .kyc
        .create_case(
            &ctx(),
            &alice,
            NewCase {
                subject_kind: SubjectKind::Organization,
                subject_ref: alice.tenant_id,
                country: "US".into(),
                number_type: NumberType::Mobile,
                ownership_class: O::Organization,
                purpose_code: "CUSTOMER_SUPPORT".into(),
                provider_id: None,
                tenant_classification: TenantClassification::External,
            },
            None,
        )
        .await
        .unwrap()
        .value;
    let evidence = w
        .kyc
        .attach_evidence(
            &ctx(),
            &alice,
            case.id,
            EvidenceKind::BusinessRegistrationCertificate,
            "application/pdf",
            EvidenceBytes::new(fx::pdf("ownership")),
            None,
        )
        .await
        .unwrap()
        .value;
    let evidence_path = format!("{path}/evidence");
    let good = json!({ "evidenceIds": [evidence.id] });
    let r = w
        .call(
            None,
            Method::POST,
            &evidence_path,
            Some(good.clone()),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(
            Some(&w.p.reader),
            Method::POST,
            &evidence_path,
            Some(good.clone()),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    let r = w
        .call(
            Some(&w.p.bob),
            Method::POST,
            &evidence_path,
            Some(good.clone()),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    for bad in [json!({}), json!({ "evidenceIds": ["passport"] })] {
        let r = w
            .call(
                Some(&w.p.alice),
                Method::POST,
                &evidence_path,
                Some(bad.clone()),
                false,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = w
        .call(
            Some(&w.p.alice),
            Method::POST,
            &evidence_path,
            Some(good),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "ELIGIBILITY_CHECK", "{:?}", r.body);
    assert_eq!(r.body["evidenceIds"], json!([evidence.id]));

    // Submit.
    let submit = format!("{path}/submit");
    let r = w.call(None, Method::POST, &submit, None, false, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(Some(&w.p.reader), Method::POST, &submit, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w
        .call(Some(&w.p.bob), Method::POST, &submit, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    w.porting.script_submit(PortOutcome::InProgress);
    let r = w
        .call(Some(&w.p.alice), Method::POST, &submit, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "IN_PROGRESS", "{:?}", r.body);
    assert_eq!(*w.porting.submissions.lock().unwrap(), 1);
    // Submitted once.
    let r = w
        .call(Some(&w.p.alice), Method::POST, &submit, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "PORT_NOT_ELIGIBLE");
    assert_eq!(
        *w.porting.submissions.lock().unwrap(),
        1,
        "never resubmitted"
    );

    // Cancel a second (port-in) request.
    let inbound = create(
        json!({ "direction": "IN", "e164": "+12025550199", "country": "US", "ownershipClass": "ORGANIZATION" }),
        "inv002-port-in-1",
    )
    .await;
    assert_eq!(inbound.status, StatusCode::CREATED, "{:?}", inbound.body);
    let inbound_id = id(&inbound.body, "portRequestId");
    // Evidence that is not the owner's KYC evidence never counts as ownership proof.
    let r = w
        .call(
            Some(&w.p.alice),
            Method::POST,
            &format!("/v1/numbers/ports/{inbound_id}/evidence"),
            Some(json!({ "evidenceIds": [Uuid::new_v4()] })),
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "DOCUMENTS_REQUIRED", "{:?}", r.body);
    assert_eq!(r.body["statusReason"], "OWNERSHIP_EVIDENCE_REQUIRED");
    let cancel = format!("/v1/numbers/ports/{inbound_id}/cancel");
    let r = w.call(None, Method::POST, &cancel, None, false, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = w
        .call(Some(&w.p.reader), Method::POST, &cancel, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w
        .call(Some(&w.p.bob), Method::POST, &cancel, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = w
        .call(
            Some(&w.p.alice),
            Method::POST,
            &format!("/v1/numbers/ports/{}/cancel", Uuid::new_v4()),
            None,
            false,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = w
        .call(Some(&w.p.alice), Method::POST, &cancel, None, false, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "CANCELLED", "{:?}", r.body);
    let r = w
        .call(Some(&w.p.alice), Method::POST, &cancel, None, false, None)
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "cancelled once: {:?}",
        r.body
    );
    assert_eq!(r.code(), "PRECONDITION_FAILED");
}

// ---------------------------------------------------------------------------------------
// Relay retirement and catalog withdrawal
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn operators_retire_relay_numbers_and_withdraw_catalog_entries() {
    let Some(w) = world(StewardshipSettings::default()).await else {
        return;
    };
    // A relay number imported by the Privacy Relay.
    let relay_ref = Uuid::new_v4();
    let r = w
        .h
        .internal_json(
            &w.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-inventory/imports",
            json!({ "relayInventoryRef": relay_ref, "e164": "+12025550178", "country": "US",
                    "capabilities": ["VOICE_INBOUND", "VOICE_OUTBOUND"], "bindingRef": "inv002-relay-1" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let retire = format!("/v1/admin/relay-numbers/{relay_ref}/retire");
    w.admin_only(Method::POST, &retire, None).await;
    let r = w
        .admin(Method::POST, "/v1/admin/relay-numbers/relay-1/retire", None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    let r = w
        .admin(
            Method::POST,
            &format!("/v1/admin/relay-numbers/{}/retire", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let r = w.admin(Method::POST, &retire, None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["relayInventoryRef"], json!(relay_ref));
    assert_eq!(r.body["status"], "RETIRED");
    // A retired relay number is never leased again.
    let lease = w
        .h
        .internal_json(
            &w.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-leases",
            json!({ "relayInventoryRef": relay_ref, "holderRef": Uuid::new_v4(), "generation": 1,
                    "expiresAt": (w.h.now() + Duration::hours(1)).to_rfc3339(), "idempotencyKey": "inv002-lease-1" }),
        )
        .await;
    assert_ne!(lease.status, StatusCode::CREATED, "{:?}", lease.body);

    // Catalog withdrawal of a current number-type entry.
    let entry = common::strings(
        w.h.conn(),
        "SELECT id::text FROM number_type_catalog WHERE status = 'CURRENT' AND country_code = 'US' \
           AND number_type = 'TOLL_FREE' LIMIT 1",
    )
    .await
    .pop()
    .or(common::strings(
        w.h.conn(),
        "SELECT id::text FROM number_type_catalog WHERE status = 'CURRENT' AND country_code = 'US' \
           ORDER BY number_type DESC LIMIT 1",
    )
    .await
    .pop())
    .expect("a current US number type");
    let withdraw = format!("/v1/admin/catalog/number-types/{entry}/withdraw");
    let reason = json!({ "reasonCode": "CATALOG_CORRECTION" });
    w.admin_only(Method::POST, &withdraw, Some(reason.clone()))
        .await;
    for bad in [
        json!({}),
        json!({ "reasonCode": "CATALOG_CORRECTION", "successor": entry }),
    ] {
        let r = w.admin(Method::POST, &withdraw, Some(bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = w
        .admin(
            Method::POST,
            "/v1/admin/catalog/number-types/toll-free/withdraw",
            Some(reason.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    let r = w
        .admin(
            Method::POST,
            &format!("/v1/admin/catalog/number-types/{}/withdraw", Uuid::new_v4()),
            Some(reason.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let before =
        w.h.scalar("SELECT COALESCE(max(revision), 0) FROM number_catalog_revisions")
            .await;
    let r = w.admin(Method::POST, &withdraw, Some(reason.clone())).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["entryId"], json!(entry));
    assert!(
        r.body["catalogRevision"].as_i64().unwrap() > before,
        "{:?}",
        r.body
    );
    assert_eq!(
        w.h.scalar(&format!(
            "SELECT count(*) FROM number_type_catalog WHERE id = '{entry}' AND status = 'SUPERSEDED'"
        ))
        .await,
        1
    );
    // Withdrawn once.
    let r = w.admin(Method::POST, &withdraw, Some(reason)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
}

// ---------------------------------------------------------------------------------------
// KYC review decisions and appeals
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn kyc_reviews_are_operator_decisions_and_rejections_are_appealed_by_the_owner() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let (_, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(ManualKycVerifier));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let pa = h.pa.clone().unwrap();
    let kyc_scopes = ["numbers.kyc.read", "numbers.kyc.manage"];
    let alice = pa.customer("SANDBOX", &kyc_scopes, &kyc_scopes).await;
    let bob = pa.customer("SANDBOX", &kyc_scopes, &kyc_scopes).await;
    let reader = pa
        .customer("SANDBOX", &["numbers.kyc.read"], &["numbers.kyc.read"])
        .await;
    let ops = pa
        .customer_as(
            "SANDBOX",
            "FIRST_PARTY",
            &["numbers.admin"],
            &["numbers.admin"],
        )
        .await;
    let call = |who: Option<&Customer>, path: &str, body: Value, operator: bool| {
        let mut b = axum::http::Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(c) = who {
            b = b.header("authorization", format!("Bearer {}", c.secret));
        }
        if operator {
            b = b.header("x-cf-operator", OPERATOR);
        }
        let req = b.body(axum::body::Body::from(body.to_string())).unwrap();
        let h = &h;
        async move { h.raw(req).await }
    };

    // An organization case under manual review in alice's scope.
    let s = owner(&alice);
    let case = kyc
        .create_case(
            &ctx(),
            &s,
            NewCase {
                subject_kind: SubjectKind::Organization,
                subject_ref: Uuid::new_v4(),
                country: "US".into(),
                number_type: NumberType::Mobile,
                ownership_class: O::Organization,
                purpose_code: "CUSTOMER_SUPPORT".into(),
                provider_id: None,
                tenant_classification: TenantClassification::External,
            },
            None,
        )
        .await
        .unwrap()
        .value;
    let mut evidence = Vec::new();
    for (kind, marker) in [
        (EvidenceKind::BusinessRegistrationCertificate, "reg"),
        (EvidenceKind::PurposeStatement, "purpose"),
    ] {
        evidence.push(
            kyc.attach_evidence(
                &ctx(),
                &s,
                case.id,
                kind,
                "application/pdf",
                EvidenceBytes::new(fx::pdf(marker)),
                None,
            )
            .await
            .unwrap()
            .value
            .id,
        );
    }
    let c = kyc.case(&s, case.id).await.unwrap();
    kyc.submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap();
    let clock: SharedClock = Arc::new(h.clock.clone());
    JobRunner::new(
        h.conn().clone(),
        clock,
        "inv002-kyc",
        Duration::seconds(30),
        RetryPolicy {
            base: Duration::seconds(2),
            max: Duration::seconds(30),
        },
    )
    .with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }))
    .run_due()
    .await
    .unwrap();
    let c = kyc.case(&s, case.id).await.unwrap();
    assert_eq!(c.status(), KycStatus::UnderReview);

    // The operator's review decision.
    let decisions = format!("/v1/admin/kyc/cases/{}/decisions", case.id);
    let reject = json!({ "decision": "REJECT", "reasonCode": "INCOMPLETE_FILING",
                         "evidenceIds": [evidence[0]], "expectedRevision": c.revision });
    let r = call(None, &decisions, reject.clone(), true).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.code(), "AUTHENTICATION_FAILED");
    let r = call(Some(&alice), &decisions, reject.clone(), true).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    let r = call(Some(&ops), &decisions, reject.clone(), false).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
    for bad in [
        json!({ "decision": "REJECT", "reasonCode": "INCOMPLETE_FILING", "expectedRevision": c.revision }),
        json!({ "decision": "MAYBE", "reasonCode": "INCOMPLETE_FILING", "evidenceIds": [evidence[0]], "expectedRevision": c.revision }),
        json!({ "decision": "REJECT", "reasonCode": "INCOMPLETE_FILING", "evidenceIds": ["reg"], "expectedRevision": c.revision }),
    ] {
        let r = call(Some(&ops), &decisions, bad.clone(), true).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = call(
        Some(&ops),
        "/v1/admin/kyc/cases/case-1/decisions",
        reject.clone(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
    let r = call(
        Some(&ops),
        &format!("/v1/admin/kyc/cases/{}/decisions", Uuid::new_v4()),
        reject.clone(),
        true,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let mut foreign = reject.clone();
    foreign["evidenceIds"] = json!([Uuid::new_v4()]);
    let r = call(Some(&ops), &decisions, foreign, true).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "EVIDENCE_NOT_IN_CASE");
    let mut stale = reject.clone();
    stale["expectedRevision"] = json!(c.revision - 1);
    let r = call(Some(&ops), &decisions, stale, true).await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "stale revision: {:?}",
        r.body
    );
    assert_eq!(r.code(), "VERSION_CONFLICT");
    assert_eq!(
        kyc.case(&s, case.id).await.unwrap().status(),
        KycStatus::UnderReview,
        "refused reviews change nothing"
    );
    let r = call(Some(&ops), &decisions, reject.clone(), true).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["case"]["status"], "REJECTED", "{:?}", r.body);
    assert_eq!(r.body["decision"]["decisionKind"], "MANUAL_REVIEW");
    assert_eq!(r.body["decision"]["actorKind"], "OPERATOR");
    assert_eq!(r.body["decision"]["evidenceIds"], json!([evidence[0]]));
    let rejection = id(&r.body["decision"], "decisionId");
    // Replaying the decision on the decided revision is refused.
    let r = call(Some(&ops), &decisions, reject, true).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
    assert_eq!(r.code(), "VERSION_CONFLICT");

    // The owner's appeal.
    let appeals = format!("/v1/numbers/kyc/cases/{}/appeals", case.id);
    let body = json!({ "reasonCode": "FILING_CORRECTED" });
    let r = call(None, &appeals, body.clone(), false).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(Some(&reader), &appeals, body.clone(), false).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    let r = call(Some(&bob), &appeals, body.clone(), false).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    for bad in [
        json!({}),
        json!({ "reasonCode": "FILING_CORRECTED", "evidence": [] }),
    ] {
        let r = call(Some(&alice), &appeals, bad.clone(), false).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {:?}", r.body);
    }
    let r = call(
        Some(&alice),
        &format!("/v1/numbers/kyc/cases/{}/appeals", Uuid::new_v4()),
        body.clone(),
        false,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    let r = call(Some(&alice), &appeals, body.clone(), false).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["status"], "OPEN");
    assert_eq!(r.body["kycCaseId"], json!(case.id));
    assert_eq!(id(&r.body, "appealedDecisionId"), rejection);
    // One open appeal per decision.
    let r = call(
        Some(&alice),
        &appeals,
        json!({ "reasonCode": "AGAIN" }),
        false,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["reason"], "NOT_APPEALABLE");
}
