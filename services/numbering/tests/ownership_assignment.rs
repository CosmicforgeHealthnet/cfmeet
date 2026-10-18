//! Part IV Workstream J: ownership and assignment — owners from trusted context
//! (organization = the authenticated tenant, principals resolved by the directory),
//! eligibility bound to the owner and re-evaluated, the assignment lifecycle up to ACTIVE
//! only on proven routing readiness, per-class target policy, employee offboarding,
//! organizational pools, same-owner transfers (cross-owner fails closed), idempotency and
//! concurrency, database guards, the customer / admin API and migration rollback.
mod common;

use std::sync::{atomic::Ordering, Arc};

use axum::http::{Method, StatusCode};
use common::{
    acquisition as ax, catalog, contracts::conforms, exec, kyc as fx, providers as px, scalar,
    scope, strings, Harness,
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
        kyc::SubjectKind,
        ownership::TransferKind,
        routing::RoutingTargets,
        Capability as C, DomainError, EnvironmentKind, LifecycleState as S, NumberType, OwnerScope,
        OwnershipClass as O, SmsTargetKind as Sms, VoiceTargetKind as V,
    },
};
use serde_json::json;
use uuid::Uuid;

const COMMON: &str = "numbering/common/v1.schema.json#/$defs/";
const GEO: [C; 3] = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];

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

fn sandbox() -> OwnerScope {
    scope(EnvironmentKind::Sandbox)
}

fn key() -> String {
    format!("key-{}", Uuid::new_v4().simple())
}

fn targets(voice: (V, Option<Uuid>), sms: (Sms, Option<Uuid>)) -> RoutingTargets {
    RoutingTargets { voice, sms }
}

fn unrouted() -> RoutingTargets {
    targets((V::None, None), (Sms::None, None))
}

struct J {
    h: Harness,
    a: Acquisition,
    dir: Arc<ax::Principals>,
    runner: numbering::infra::jobs::JobRunner,
}

async fn setup() -> Option<J> {
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
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let dir = Arc::new(ax::Principals::default());
    let a = ax::of(&h, &p, AcquisitionSettings::default()).with_principals(dir.clone());
    let runner = ax::runner(&h, &p, &a);
    Some(J { h, a, dir, runner })
}

impl J {
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
                    number_type: NumberType::Geographic,
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

    async fn org_decision(&self, s: &OwnerScope, class: O, number: Uuid, caps: &[C]) -> Uuid {
        self.decide(
            s,
            (SubjectKind::Organization, s.tenant_id),
            class,
            caps,
            number,
        )
        .await
    }

    /// A geographic number HELD for the caller (organization decision).
    async fn held(&self, s: &OwnerScope, external_ref: &str, caps: &[C]) -> (Uuid, Uuid) {
        let id = self.id_of(s, external_ref).await;
        let decision = self.org_decision(s, O::Organization, id, caps).await;
        let r = self
            .a
            .create_reservation(
                &ctx(),
                s,
                ReservationRequest {
                    number_resource_id: id,
                    capabilities: caps.to_vec(),
                    hold_seconds: Some(3_600),
                    eligibility_decision_id: Some(decision),
                },
                &key(),
            )
            .await
            .unwrap()
            .reservation;
        self.drain().await;
        (id, r.id)
    }

    async fn provision(&self, s: &OwnerScope, reservation: Uuid, caps: &[C]) {
        self.a
            .provision(
                &ctx(),
                s,
                standing(),
                ProvisionRequest {
                    reservation_id: reservation,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: None,
                },
                &key(),
            )
            .await
            .unwrap();
        self.drain().await;
    }

    /// A PROVISIONED acquisition: (number, acquisition).
    async fn acquired(&self, s: &OwnerScope, external_ref: &str, caps: &[C]) -> (Uuid, Uuid) {
        let (id, rid) = self.held(s, external_ref, caps).await;
        self.provision(s, rid, caps).await;
        let acq = self
            .h
            .numbers
            .acquisition_of_reservation(rid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acq.status, "PROVISIONED");
        (id, acq.id)
    }

    /// Provider routes configured for `caps` and acknowledged by each consumer.
    async fn ready(&self, s: &OwnerScope, id: Uuid, acq: Uuid, caps: &[C]) {
        self.a
            .configure_routes(&ctx(), s, acq, caps, &key())
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
    }

    #[allow(clippy::too_many_arguments)]
    async fn assign(
        &self,
        s: &OwnerScope,
        basis: AssignmentBasis,
        class: O,
        owner: Option<Uuid>,
        routing: RoutingTargets,
        caps: &[C],
        decision: Option<Uuid>,
        idempotency_key: &str,
    ) -> Result<AssignmentRow, DomainError> {
        self.a
            .create_assignment(
                &ctx(),
                s,
                standing(),
                AssignmentRequest {
                    basis,
                    ownership_class: class,
                    owner_principal_id: owner,
                    targets: routing,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: decision,
                    pool_id: None,
                },
                idempotency_key,
            )
            .await
            .map(|a| a.assignment)
    }

    /// An ACTIVE assignment of `class` on a fresh number.
    async fn active(
        &self,
        s: &OwnerScope,
        external_ref: &str,
        caps: &[C],
        class: O,
        owner: Option<Uuid>,
        routing: RoutingTargets,
    ) -> (Uuid, AssignmentRow) {
        let (id, acq) = self.acquired(s, external_ref, caps).await;
        let decision = match owner {
            Some(p) => Some(
                self.decide(s, (SubjectKind::Individual, p), class, caps, id)
                    .await,
            ),
            None => Some(self.org_decision(s, class, id, caps).await),
        };
        let a = self
            .assign(
                s,
                AssignmentBasis::Acquisition(acq),
                class,
                owner,
                routing,
                caps,
                decision,
                &key(),
            )
            .await
            .unwrap();
        self.ready(s, id, acq, caps).await;
        let a = self
            .a
            .activate_assignment(&ctx(), s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
        assert_eq!(a.status, "ACTIVE");
        (id, a)
    }

    async fn state(&self, id: Uuid) -> S {
        self.h.numbers.resource(id).await.unwrap().lifecycle_state()
    }

    async fn history(&self, s: &OwnerScope, assignment: Uuid) -> Vec<(String, String, String)> {
        self.h
            .numbers
            .assignment_history(s, assignment)
            .await
            .unwrap()
            .into_iter()
            .map(|h| (h.event_kind, h.to_status, h.reason_code))
            .collect()
    }
}

fn code(e: DomainError) -> &'static str {
    e.code()
}

// ---------------------------------------------------------------------------------------
// Steps 10–11: assignment creation and activation
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_organization_assignment_becomes_active_only_once_every_route_is_ready() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.acquired(&s, "syn-voice-sms-geo", &GEO).await;
    let queue = Uuid::new_v4();
    let dept = Uuid::new_v4();
    let decision = t.org_decision(&s, O::Organization, id, &GEO).await;
    let a = t
        .assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(queue)),
                (Sms::Department, Some(dept)),
            ),
            &GEO,
            Some(decision),
            "assign-org-0001",
        )
        .await
        .unwrap();
    // The legal owner is the authenticated organization; nothing in the request named it.
    assert_eq!(
        (a.status.as_str(), a.owner_kind.as_str(), a.owner_ref),
        ("RESERVED", "ORGANIZATION", s.tenant_id)
    );
    assert_eq!(a.routing_profile_revision, Some(1));
    // Eligibility was re-evaluated: the recorded basis is a NEW decision.
    let recorded = a.eligibility_decision_ref.unwrap();
    assert_ne!(recorded, decision);
    assert_eq!(
        t.h.numbers
            .eligibility_decision(&s, recorded)
            .await
            .unwrap()
            .outcome,
        "ELIGIBLE"
    );
    let steps: Vec<String> = t
        .history(&s, a.id)
        .await
        .into_iter()
        .map(|(k, to, _)| format!("{k}:{to}"))
        .collect();
    assert_eq!(
        steps,
        [
            "CREATED:REQUESTED",
            "STATUS_CHANGED:PENDING_ELIGIBILITY",
            "STATUS_CHANGED:ELIGIBLE",
            "STATUS_CHANGED:RESERVED",
            "ROUTING_CHANGED:RESERVED"
        ]
    );
    // A provisioned number with an assignment is still not ACTIVE: no route is ready.
    let err =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap_err();
    assert_eq!(code(err), "ROUTING_NOT_READY");
    t.a.configure_routes(&ctx(), &s, acq, &GEO, "assign-org-routes")
        .await
        .unwrap();
    t.drain().await;
    // Provider-configured but unacknowledged by the consumers: still not ready.
    let err =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap_err();
    assert!(err.to_string().contains("CONSUMER_NOT_READY"), "{err}");
    let routes = t.h.numbers.current_routes(id).await.unwrap();
    let voice = routes.iter().find(|r| r.family == "VOICE").unwrap();
    let r = t
        .h
        .internal_json(
            &t.h.peers.telephony,
            Method::POST,
            &format!("/internal/v1/number-resources/{id}/route-readiness"),
            json!({ "family": "VOICE", "routeRef": voice.route_ref, "routeRevision": voice.route_revision }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // Voice alone is not enough: SMS is routed too.
    let err =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap_err();
    assert!(err.to_string().contains("SMS: CONSUMER_NOT_READY"), "{err}");
    assert_eq!(t.state(id).await, S::Provisioned);
    let sms = routes.iter().find(|r| r.family == "SMS").unwrap();
    t.h.internal_json(
        &t.h.peers.sms_gateway,
        Method::POST,
        &format!("/internal/v1/number-resources/{id}/route-readiness"),
        json!({ "family": "SMS", "routeRef": sms.route_ref, "routeRevision": sms.route_revision }),
    )
    .await;
    let active =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap();
    assert!(!active.replayed);
    assert_eq!(active.assignment.status, "ACTIVE");
    assert!(active.assignment.activated_at.is_some());
    assert_eq!(t.state(id).await, S::Active);
    // Activation is naturally idempotent.
    let again =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap();
    assert!(again.replayed);
    assert_eq!(again.assignment.revision, active.assignment.revision);
}

#[tokio::test]
async fn a_held_reservation_is_assigned_first_and_activated_after_provisioning() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid) = t.held(&s, "syn-voice-sms-geo", &GEO).await;
    let a = t
        .assign(
            &s,
            AssignmentBasis::Reservation(rid),
            O::Organization,
            None,
            targets(
                (V::EnterpriseIvr, Some(Uuid::new_v4())),
                (Sms::Application, Some(Uuid::new_v4())),
            ),
            &GEO,
            None,
            &key(),
        )
        .await
        .unwrap();
    assert_eq!(a.reservation_id, Some(rid));
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(
        (r.status.as_str(), r.converted_to.as_deref()),
        ("CONVERTED", Some("ASSIGNMENT"))
    );
    // Never ACTIVE because a number exists: provisioning comes first.
    assert_eq!(
        code(
            t.a.activate_assignment(&ctx(), &s, standing(), a.id)
                .await
                .unwrap_err()
        ),
        "PROVISIONING_REQUIRED"
    );
    t.provision(&s, rid, &GEO).await;
    assert_eq!(t.state(id).await, S::Provisioned);
    let acq =
        t.h.numbers
            .acquisition_of_reservation(rid)
            .await
            .unwrap()
            .unwrap();
    t.ready(&s, id, acq.id, &GEO).await;
    let active =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
    assert_eq!(active.status, "ACTIVE");
    assert_eq!(t.state(id).await, S::Active);

    // A reservation whose provisioning is in flight is not assigned around it.
    let caps = [C::VoiceInbound, C::VoiceOutbound];
    let (_, rid2) = t.held(&s, "syn-voice-only", &caps).await;
    t.a.provision(
        &ctx(),
        &s,
        standing(),
        ProvisionRequest {
            reservation_id: rid2,
            capabilities: caps.to_vec(),
            eligibility_decision_id: None,
        },
        &key(),
    )
    .await
    .unwrap();
    let err = t
        .assign(
            &s,
            AssignmentBasis::Reservation(rid2),
            O::Organization,
            None,
            unrouted(),
            &caps,
            None,
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "ACQUISITION_IN_PROGRESS");
}

#[tokio::test]
async fn assignment_creation_is_idempotent_exclusive_and_concurrency_safe() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.acquired(&s, "syn-voice-sms-geo", &GEO).await;
    let route = targets(
        (V::EnterpriseQueue, Some(Uuid::new_v4())),
        (Sms::None, None),
    );
    let decision = t.org_decision(&s, O::Organization, id, &GEO).await;
    let (a, b) = tokio::join!(
        t.assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::Organization,
            None,
            route,
            &GEO,
            Some(decision),
            "race-key-0001"
        ),
        t.assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::Organization,
            None,
            route,
            &GEO,
            Some(decision),
            "race-key-0002"
        ),
    );
    let (won, lost) = match (a, b) {
        (Ok(w), Err(l)) | (Err(l), Ok(w)) => (w, l),
        other => panic!("exactly one assignment wins: {other:?}"),
    };
    assert_eq!(code(lost), "ASSIGNMENT_EXISTS");
    assert_eq!(t.h.scalar("SELECT count(*) FROM dcn_assignments").await, 1);
    // Replays return the stored assignment; a different request under the key is refused.
    // The loser's key recorded nothing (its transaction rolled back): only the winner's
    // key replays.
    let mut winner_key = String::new();
    for k in ["race-key-0001", "race-key-0002"] {
        if let Ok(a) = t
            .assign(
                &s,
                AssignmentBasis::Acquisition(acq),
                O::Organization,
                None,
                route,
                &GEO,
                Some(decision),
                k,
            )
            .await
        {
            assert_eq!(a.id, won.id);
            winner_key = k.to_string();
        }
    }
    assert!(!winner_key.is_empty());
    let replay =
        t.a.create_assignment(
            &ctx(),
            &s,
            standing(),
            AssignmentRequest {
                basis: AssignmentBasis::Acquisition(acq),
                ownership_class: O::Organization,
                owner_principal_id: None,
                targets: route,
                capabilities: GEO.to_vec(),
                eligibility_decision_id: Some(decision),
                pool_id: None,
            },
            &winner_key,
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.assignment.id, won.id);
    let err = t
        .assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::Department,
            None,
            route,
            &GEO,
            Some(decision),
            &winner_key,
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "IDEMPOTENCY_KEY_REUSED");
    // Another scope cannot assign (or even see) the acquisition.
    let other = sandbox();
    let err = t
        .assign(
            &other,
            AssignmentBasis::Acquisition(acq),
            O::Organization,
            None,
            route,
            &GEO,
            None,
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "NOT_FOUND");
    // Capabilities that were not provisioned are refused.
    let err = t
        .assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::Organization,
            None,
            route,
            &[C::SmsOutbound],
            Some(decision),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "CAPABILITY_NOT_SUPPORTED");
}

// ---------------------------------------------------------------------------------------
// Step 10: ownership classes, owners and target policy
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_personal_number_belongs_to_a_resolved_active_principal_and_routes_to_them_only() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.acquired(&s, "syn-voice-sms-geo", &GEO).await;
    let alice = t.dir.add(P::Active);
    let suspended = t.dir.add(P::Inactive);
    let stranger = Uuid::new_v4();
    let mine = targets((V::Principal, Some(alice)), (Sms::Principal, Some(alice)));
    let basis = AssignmentBasis::Acquisition(acq);
    let alice_decision = t
        .decide(&s, (SubjectKind::Individual, alice), O::Personal, &GEO, id)
        .await;
    for (class, owner, routing, decision, expected) in [
        // The owner is named for personal numbers and never for organizational ones.
        (
            O::Personal,
            None,
            mine,
            Some(alice_decision),
            "VALIDATION_ERROR",
        ),
        (
            O::Organization,
            Some(alice),
            unrouted(),
            None,
            "VALIDATION_ERROR",
        ),
        // Owners are principals the directory resolves ACTIVE in this environment.
        (
            O::Personal,
            Some(suspended),
            unrouted(),
            Some(alice_decision),
            "PRINCIPAL_NOT_ACTIVE",
        ),
        (
            O::Personal,
            Some(stranger),
            unrouted(),
            Some(alice_decision),
            "PRINCIPAL_NOT_FOUND",
        ),
        // A personal number routes to its owner only.
        (
            O::Personal,
            Some(alice),
            targets((V::Principal, Some(stranger)), (Sms::None, None)),
            Some(alice_decision),
            "TARGET_NOT_PERMITTED",
        ),
        (
            O::Personal,
            Some(alice),
            targets((V::EnterpriseQueue, Some(stranger)), (Sms::None, None)),
            Some(alice_decision),
            "TARGET_NOT_PERMITTED",
        ),
        // Eligibility is bound to the owner and class: the reservation's organization
        // decision does not make a personal number.
        (
            O::Personal,
            Some(alice),
            mine,
            None,
            "ELIGIBILITY_SUBJECT_MISMATCH",
        ),
    ] {
        let err = t
            .assign(&s, basis, class, owner, routing, &GEO, decision, &key())
            .await
            .unwrap_err();
        assert_eq!(code(err), expected, "{class} {owner:?}");
    }
    let other_person = t.dir.add(P::Active);
    let theirs = t
        .decide(
            &s,
            (SubjectKind::Individual, other_person),
            O::Personal,
            &GEO,
            id,
        )
        .await;
    let err = t
        .assign(
            &s,
            basis,
            O::Personal,
            Some(alice),
            mine,
            &GEO,
            Some(theirs),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "ELIGIBILITY_SUBJECT_MISMATCH");
    // The directory unreachable: fail closed.
    t.dir.unavailable.store(true, Ordering::SeqCst);
    let err = t
        .assign(
            &s,
            basis,
            O::Personal,
            Some(alice),
            mine,
            &GEO,
            Some(alice_decision),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "PRINCIPAL_UNVERIFIED");
    t.dir.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(t.h.scalar("SELECT count(*) FROM dcn_assignments").await, 0);
    let a = t
        .assign(
            &s,
            basis,
            O::Personal,
            Some(alice),
            mine,
            &GEO,
            Some(alice_decision),
            &key(),
        )
        .await
        .unwrap();
    assert_eq!(
        (
            a.owner_kind.as_str(),
            a.owner_ref,
            a.ownership_class.as_str()
        ),
        ("PRINCIPAL", alice, "PERSONAL")
    );
    // Voicemail and approved forwarding are personal routes too; a queue never is.
    let profile = a.routing_profile_id.unwrap();
    let err =
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
        .unwrap_err();
    assert_eq!(code(err), "TARGET_NOT_PERMITTED");
    let a =
        t.a.update_routing(
            &ctx(),
            &s,
            a.id,
            1,
            targets(
                (V::Voicemail, Some(Uuid::new_v4())),
                (Sms::Principal, Some(alice)),
            ),
            "ROUTING_CHANGED",
        )
        .await
        .unwrap();
    assert_eq!(
        (a.routing_profile_id, a.routing_profile_revision),
        (Some(profile), Some(2))
    );
    // The owner deactivated before activation: activation refuses.
    t.ready(&s, id, acq, &GEO).await;
    t.dir.set(alice, P::Inactive);
    let err =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap_err();
    assert_eq!(code(err), "PRINCIPAL_NOT_ACTIVE");
    t.dir.set(alice, P::Active);
    assert_eq!(
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap()
            .assignment
            .status,
        "ACTIVE"
    );
}

#[tokio::test]
async fn eligibility_is_re_evaluated_at_activation_and_a_revoked_basis_never_activates() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.acquired(&s, "syn-voice-sms-geo", &GEO).await;
    let decision = t.org_decision(&s, O::Department, id, &GEO).await;
    let a = t
        .assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::Department,
            None,
            targets(
                (V::EnterpriseDepartment, Some(Uuid::new_v4())),
                (Sms::Department, Some(Uuid::new_v4())),
            ),
            &GEO,
            Some(decision),
            &key(),
        )
        .await
        .unwrap();
    t.ready(&s, id, acq, &GEO).await;
    // The operator disables number eligibility: the same assignment no longer activates.
    let disabled = Acquisition::new(
        t.h.numbers.clone(),
        t.h.state.providers.clone(),
        Arc::new(EligibilityPolicy {
            enabled: false,
            ..Default::default()
        }),
        AcquisitionSettings::default(),
    )
    .with_principals(t.dir.clone());
    let err = disabled
        .activate_assignment(&ctx(), &s, standing(), a.id)
        .await
        .unwrap_err();
    assert_eq!(code(err), "ELIGIBILITY_REQUIRED");
    // A suspended tenant standing refuses too.
    let err =
        t.a.activate_assignment(
            &ctx(),
            &s,
            Standing {
                active: false,
                ..standing()
            },
            a.id,
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "ELIGIBILITY_REQUIRED");
    assert_eq!(
        t.h.numbers.assignment(&s, a.id).await.unwrap().status,
        "RESERVED"
    );
    assert_eq!(t.state(id).await, S::Provisioned);
    assert_eq!(
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap()
            .assignment
            .status,
        "ACTIVE"
    );
}

// ---------------------------------------------------------------------------------------
// Step 12: employee numbers and offboarding
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_employee_direct_number_stays_organizational_and_offboarding_moves_it() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let e1 = t.dir.add(P::Active);
    let e2 = t.dir.add(P::Active);
    let gone = t.dir.add(P::Inactive);
    let (id, acq) = t.acquired(&s, "syn-voice-sms-geo", &GEO).await;
    let decision = t.org_decision(&s, O::EmployeeDirect, id, &GEO).await;
    let err = t
        .assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(gone)), (Sms::None, None)),
            &GEO,
            Some(decision),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "PRINCIPAL_NOT_ACTIVE");
    // One employee per direct line; an organization queue is not a direct line.
    for routing in [
        targets((V::Principal, Some(e1)), (Sms::Principal, Some(e2))),
        targets((V::EnterpriseQueue, Some(e1)), (Sms::None, None)),
    ] {
        let err = t
            .assign(
                &s,
                AssignmentBasis::Acquisition(acq),
                O::EmployeeDirect,
                None,
                routing,
                &GEO,
                Some(decision),
                &key(),
            )
            .await
            .unwrap_err();
        assert_eq!(code(err), "TARGET_NOT_PERMITTED");
    }
    let a = t
        .assign(
            &s,
            AssignmentBasis::Acquisition(acq),
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(e1)), (Sms::Principal, Some(e1))),
            &GEO,
            Some(decision),
            &key(),
        )
        .await
        .unwrap();
    // The employee is where it routes, never who owns it.
    assert_eq!(
        (a.owner_kind.as_str(), a.owner_ref),
        ("ORGANIZATION", s.tenant_id)
    );
    t.ready(&s, id, acq, &GEO).await;
    let a =
        t.a.activate_assignment(&ctx(), &s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
    let before = t.h.numbers.resource(id).await.unwrap().routing_revision;
    // Offboarding moves the number away from the departing employee.
    let err =
        t.a.offboard_employee(
            &ctx(),
            &s,
            a.id,
            1,
            Some(targets((V::Principal, Some(e1)), (Sms::None, None))),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "TARGET_NOT_PERMITTED");
    let err =
        t.a.offboard_employee(&ctx(), &s, a.id, 7, None)
            .await
            .unwrap_err();
    assert_eq!(code(err), "VERSION_CONFLICT");
    let off =
        t.a.offboard_employee(&ctx(), &s, a.id, 1, None)
            .await
            .unwrap();
    assert_eq!(
        (
            off.status.as_str(),
            off.owner_ref,
            off.routing_profile_revision
        ),
        ("ACTIVE", s.tenant_id, Some(2))
    );
    let profile =
        t.h.numbers
            .routing_profile_revision(off.routing_profile_id.unwrap(), 2)
            .await
            .unwrap();
    assert_eq!(
        (
            profile.voice_target_kind.as_str(),
            profile.sms_target_kind.as_str()
        ),
        ("NONE", "NONE")
    );
    // Every routing change advances the number's routing revision (grants bound to the
    // previous revision stop verifying — Workstream K).
    assert!(t.h.numbers.resource(id).await.unwrap().routing_revision > before);
    let last = t.history(&s, a.id).await.pop().unwrap();
    assert_eq!(
        (last.0.as_str(), last.2.as_str()),
        ("ROUTING_CHANGED", "EMPLOYEE_OFFBOARDED")
    );
    // A replacement employee takes the line over.
    let next =
        t.a.offboard_employee(
            &ctx(),
            &s,
            a.id,
            2,
            Some(targets(
                (V::Principal, Some(e2)),
                (Sms::Principal, Some(e2)),
            )),
        )
        .await
        .unwrap();
    assert_eq!(next.routing_profile_revision, Some(3));
    // Ownership never changed along the way.
    assert!(t
        .h
        .numbers
        .assignment_history(&s, a.id)
        .await
        .unwrap()
        .iter()
        .all(|h| h.owner_ref == s.tenant_id && h.ownership_class == "EMPLOYEE_DIRECT"));
}

// ---------------------------------------------------------------------------------------
// Step 13: pools, transfers
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn organizational_pools_hold_organization_numbers_with_history() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let actor = customer_actor(&s);
    let e1 = t.dir.add(P::Active);
    let usage = Uuid::new_v4();
    let pool =
        t.h.numbers
            .create_pool(&ctx(), &s, &actor, "Sales lines", Some(usage), None)
            .await
            .unwrap();
    assert_eq!(
        code(
            t.h.numbers
                .create_pool(&ctx(), &s, &actor, "Sales lines", None, None)
                .await
                .unwrap_err()
        ),
        "POOL_EXISTS"
    );
    assert_eq!(
        code(
            t.h.numbers
                .create_pool(&ctx(), &s, &actor, "  ", None, None)
                .await
                .unwrap_err()
        ),
        "VALIDATION_ERROR"
    );
    let second =
        t.h.numbers
            .create_pool(&ctx(), &s, &actor, "Support lines", None, None)
            .await
            .unwrap();
    let (_, employee) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            &GEO,
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(e1)), (Sms::Principal, Some(e1))),
        )
        .await;
    let alice = t.dir.add(P::Active);
    let caps = [C::VoiceInbound, C::VoiceOutbound];
    let (_, personal) = t
        .active(
            &s,
            "syn-voice-only",
            &caps,
            O::Personal,
            Some(alice),
            targets((V::Principal, Some(alice)), (Sms::None, None)),
        )
        .await;
    assert_eq!(
        code(
            t.h.numbers
                .add_pool_member(&ctx(), &s, &actor, pool.id, personal.id)
                .await
                .unwrap_err()
        ),
        "POOL_MEMBER_NOT_ORGANIZATIONAL"
    );
    t.h.numbers
        .add_pool_member(&ctx(), &s, &actor, pool.id, employee.id)
        .await
        .unwrap();
    for p in [pool.id, second.id] {
        assert_eq!(
            code(
                t.h.numbers
                    .add_pool_member(&ctx(), &s, &actor, p, employee.id)
                    .await
                    .unwrap_err()
            ),
            "POOL_MEMBER_EXISTS"
        );
    }
    let members = t.h.numbers.pool_members(&s, pool.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert!(members[0].assigned());
    // Offboarded: the number is available in the pool for reassignment.
    t.a.offboard_employee(&ctx(), &s, employee.id, 1, None)
        .await
        .unwrap();
    assert!(!t.h.numbers.pool_members(&s, pool.id).await.unwrap()[0].assigned());
    // Other scopes see no pool.
    let other = sandbox();
    assert_eq!(
        code(t.h.numbers.pool(&other, pool.id).await.unwrap_err()),
        "NOT_FOUND"
    );
    assert!(t.h.numbers.pools(&other).await.unwrap().is_empty());
    // Removal ends the membership; history is kept.
    t.h.numbers
        .remove_pool_member(&ctx(), &s, &actor, pool.id, employee.id)
        .await
        .unwrap();
    assert!(t
        .h
        .numbers
        .pool_members(&s, pool.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        code(
            t.h.numbers
                .remove_pool_member(&ctx(), &s, &actor, pool.id, employee.id)
                .await
                .unwrap_err()
        ),
        "NOT_FOUND"
    );
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_pool_members WHERE removed_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(
        t.h.scalar(
            "SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.pool.changed'"
        )
        .await,
        4
    );
}

#[tokio::test]
async fn transfers_keep_the_legal_owner_and_a_class_change_opens_a_linked_successor() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let actor = customer_actor(&s);
    let e1 = t.dir.add(P::Active);
    let e2 = t.dir.add(P::Active);
    let (id, a) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            &GEO,
            O::EmployeeDirect,
            None,
            targets((V::Principal, Some(e1)), (Sms::Principal, Some(e1))),
        )
        .await;
    let pool =
        t.h.numbers
            .create_pool(&ctx(), &s, &actor, "Front desk", None, None)
            .await
            .unwrap();
    t.h.numbers
        .add_pool_member(&ctx(), &s, &actor, pool.id, a.id)
        .await
        .unwrap();
    let transfer = |class, routing, decision, expected| TransferRequest {
        ownership_class: class,
        owner_principal_id: None,
        targets: routing,
        eligibility_decision_id: decision,
        expected_routing_revision: expected,
    };
    // Same class, same owner: a routing revision on the same assignment.
    let moved =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(
                O::EmployeeDirect,
                targets((V::Principal, Some(e2)), (Sms::Principal, Some(e2))),
                None,
                1,
            ),
            "xfer-key-0001",
        )
        .await
        .unwrap();
    assert_eq!(moved.kind, TransferKind::Retarget);
    assert_eq!(
        (
            moved.assignment.id,
            moved.assignment.routing_profile_revision
        ),
        (a.id, Some(2))
    );
    // Another class of the same owner: requires eligibility of that class.
    let department = Uuid::new_v4();
    let to_dept = targets(
        (V::EnterpriseDepartment, Some(department)),
        (Sms::Department, Some(department)),
    );
    let err =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(O::Department, to_dept, None, 2),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "ELIGIBILITY_REQUIRED");
    let wrong = t.org_decision(&s, O::Campaign, id, &GEO).await;
    let err =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(O::Department, to_dept, Some(wrong), 2),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "ELIGIBILITY_SUBJECT_MISMATCH");
    let err =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(
                O::Department,
                targets((V::Principal, Some(e2)), (Sms::None, None)),
                None,
                2,
            ),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "TARGET_NOT_PERMITTED");
    let decision = t.org_decision(&s, O::Department, id, &GEO).await;
    let err =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(O::Department, to_dept, Some(decision), 1),
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "VERSION_CONFLICT");
    let out =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(O::Department, to_dept, Some(decision), 2),
            "xfer-key-0002",
        )
        .await
        .unwrap();
    assert_eq!(out.kind, TransferKind::Reclassify);
    let successor = out.assignment;
    let predecessor = out.predecessor.unwrap();
    assert_eq!(
        (
            predecessor.status.as_str(),
            predecessor.close_reason.as_deref(),
            predecessor.successor_assignment_id
        ),
        ("CLOSED", Some("TRANSFERRED_INTERNALLY"), Some(successor.id))
    );
    assert_eq!(
        (
            successor.status.as_str(),
            successor.ownership_class.as_str(),
            successor.owner_ref,
            successor.predecessor_assignment_id,
            successor.reservation_id
        ),
        (
            "ACTIVE",
            "DEPARTMENT",
            s.tenant_id,
            Some(a.id),
            a.reservation_id
        )
    );
    // The number never stopped being ACTIVE; the pool membership moved with it.
    assert_eq!(t.state(id).await, S::Active);
    let members = t.h.numbers.pool_members(&s, pool.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].assignment.id, successor.id);
    assert_eq!(
        t.history(&s, a.id).await.pop().unwrap(),
        (
            "STATUS_CHANGED".into(),
            "CLOSED".into(),
            "TRANSFERRED_INTERNALLY".into()
        )
    );
    // Replays return the same successor; nothing is transferred twice.
    let replay =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            transfer(O::Department, to_dept, Some(decision), 2),
            "xfer-key-0002",
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.assignment.id, successor.id);
    assert_eq!(
        t.h.scalar(&format!(
            "SELECT count(*) FROM dcn_assignments WHERE number_resource_id = '{id}'"
        ))
        .await,
        2
    );
    // The successor is not an employee line any more.
    assert_eq!(
        code(
            t.a.offboard_employee(&ctx(), &s, successor.id, 1, None)
                .await
                .unwrap_err()
        ),
        "NOT_AN_EMPLOYEE_NUMBER"
    );
    // The closed predecessor is not transferable.
    assert_eq!(
        code(
            t.a.transfer_assignment(
                &ctx(),
                &s,
                standing(),
                a.id,
                transfer(O::SharedTeam, unrouted(), Some(decision), 2),
                &key()
            )
            .await
            .unwrap_err()
        ),
        "ASSIGNMENT_NOT_ACTIVE"
    );
}

#[tokio::test]
async fn a_transfer_to_another_legal_owner_fails_closed_and_changes_nothing() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let alice = t.dir.add(P::Active);
    let bob = t.dir.add(P::Active);
    let (_, org) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            &GEO,
            O::Organization,
            None,
            targets(
                (V::EnterpriseQueue, Some(Uuid::new_v4())),
                (Sms::None, None),
            ),
        )
        .await;
    let caps = [C::VoiceInbound, C::VoiceOutbound];
    let (pid, personal) = t
        .active(
            &s,
            "syn-voice-only",
            &caps,
            O::Personal,
            Some(alice),
            targets((V::Principal, Some(alice)), (Sms::None, None)),
        )
        .await;
    let outbox = t.h.outbox_count().await;
    let attempt = |class, owner| TransferRequest {
        ownership_class: class,
        owner_principal_id: owner,
        targets: unrouted(),
        eligibility_decision_id: None,
        expected_routing_revision: 1,
    };
    for (assignment, req) in [
        // An organization's number never becomes a person's …
        (org.id, attempt(O::Personal, Some(alice))),
        (org.id, attempt(O::Professional, Some(bob))),
        // … a person's number never becomes the employer's …
        (personal.id, attempt(O::EmployeeDirect, None)),
        (personal.id, attempt(O::Organization, None)),
        // … nor another person's.
        (personal.id, attempt(O::Personal, Some(bob))),
    ] {
        let err =
            t.a.transfer_assignment(&ctx(), &s, standing(), assignment, req, &key())
                .await
                .unwrap_err();
        assert_eq!(code(err), "CROSS_OWNER_TRANSFER_UNSUPPORTED");
    }
    assert_eq!(t.h.outbox_count().await, outbox);
    assert_eq!(t.h.numbers.assignment(&s, org.id).await.unwrap(), org);
    assert_eq!(
        t.h.numbers.assignment(&s, personal.id).await.unwrap(),
        personal
    );
    // The same person's other class is a permitted reclassification.
    let decision = t
        .decide(
            &s,
            (SubjectKind::Individual, alice),
            O::Professional,
            &caps,
            pid,
        )
        .await;
    let out =
        t.a.transfer_assignment(
            &ctx(),
            &s,
            standing(),
            personal.id,
            TransferRequest {
                ownership_class: O::Professional,
                owner_principal_id: Some(alice),
                targets: targets(
                    (V::ProfessionalIdentity, Some(Uuid::new_v4())),
                    (Sms::None, None),
                ),
                eligibility_decision_id: Some(decision),
                expected_routing_revision: 1,
            },
            &key(),
        )
        .await
        .unwrap();
    assert_eq!(
        (
            out.assignment.ownership_class.as_str(),
            out.assignment.owner_ref
        ),
        ("PROFESSIONAL", alice)
    );
}

// ---------------------------------------------------------------------------------------
// Database guards and migration rollback
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_database_guards_ownership_transfers_and_pools() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let alice = t.dir.add(P::Active);
    let caps = [C::VoiceInbound, C::VoiceOutbound];
    let (_, org) = t
        .active(
            &s,
            "syn-voice-sms-geo",
            &GEO,
            O::Organization,
            None,
            unrouted(),
        )
        .await;
    let (_, personal) = t
        .active(
            &s,
            "syn-voice-only",
            &caps,
            O::Personal,
            Some(alice),
            targets((V::Principal, Some(alice)), (Sms::None, None)),
        )
        .await;
    let pool =
        t.h.numbers
            .create_pool(&ctx(), &s, &customer_actor(&s), "guarded", None, None)
            .await
            .unwrap();
    let flag = "SELECT set_config('numbering.assignment', 'transition', true)";
    let cases: Vec<(Vec<String>, &str)> = vec![
        // ACTIVE closes directly only by an internal transfer with a successor.
        (
            vec![
                flag.into(),
                format!("UPDATE dcn_assignments SET status = 'CLOSED', closed_at = now(), close_reason = 'MANUAL', revision = revision + 1 WHERE id = '{}'", org.id),
            ],
            "internal transfer",
        ),
        // A successor of another legal owner is refused.
        (
            vec![
                flag.into(),
                format!("UPDATE dcn_assignments SET status = 'CLOSED', closed_at = now(), close_reason = 'TRANSFERRED_INTERNALLY', \
                   successor_assignment_id = '00000000-0000-0000-0000-0000000000aa', revision = revision + 1 WHERE id = '{}'", personal.id),
                format!("INSERT INTO dcn_assignments (id, number_resource_id, tenant_id, application_id, environment_id, environment_kind, \
                   owner_kind, owner_ref, ownership_class, capability_snapshot, status, revision, requested_at, created_by, updated_at, \
                   predecessor_assignment_id) SELECT '00000000-0000-0000-0000-0000000000aa', number_resource_id, tenant_id, application_id, \
                   environment_id, environment_kind, 'ORGANIZATION', tenant_id, 'ORGANIZATION', capability_snapshot, 'REQUESTED', 1, now(), \
                   'test', now(), id FROM dcn_assignments WHERE id = '{}'", personal.id),
            ],
            "legal owner",
        ),
        // Only organization-owned numbers join a pool.
        (
            vec![format!(
                "INSERT INTO number_pool_members (id, pool_id, assignment_id, number_resource_id, tenant_id, application_id, \
                   environment_id, added_by, added_at) SELECT gen_random_uuid(), '{}', id, number_resource_id, tenant_id, \
                   application_id, environment_id, 'test', now() FROM dcn_assignments WHERE id = '{}'",
                pool.id, personal.id
            )],
            "organization",
        ),
        // Pool memberships are history: never deleted.
        (
            vec![format!("DELETE FROM number_pools WHERE id = '{}'", pool.id)],
            "never deleted",
        ),
    ];
    for (statements, expected) in cases {
        let refs: Vec<&str> = statements.iter().map(String::as_str).collect();
        let err = common::in_txn(t.h.conn(), &refs)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "{statements:?}: {err}");
    }
    use numbering_migration::{Migrator, MigratorTrait};
    let steps = common::steps_down_to("m20261022_000008_ownership_assignment");
    let err = Migrator::down(t.h.conn(), Some(steps)).await.unwrap_err();
    assert!(err.to_string().contains("Workstream J"), "{err}");
    let Some(clean) = common::TestDb::new().await else {
        return;
    };
    Migrator::down(&clean.conn, Some(steps)).await.unwrap();
    assert_eq!(
        scalar(
            &clean.conn,
            "SELECT count(*) FROM information_schema.tables WHERE table_name LIKE 'number_pool%'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&clean.conn, "SELECT count(*) FROM information_schema.columns WHERE table_name = 'dcn_assignments' AND column_name LIKE '%cessor_assignment_id'").await,
        0
    );
    Migrator::up(&clean.conn, None).await.unwrap();
    let _ = exec(&clean.conn, "SELECT 1").await;
}

// ---------------------------------------------------------------------------------------
// Step 14: the customer and administration API
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_assignment_api_is_scoped_object_checked_idempotent_and_contract_conformant() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let a = ax::of(&h, &p, AcquisitionSettings::default());
    let runner = ax::runner(&h, &p, &a);
    let pa = h.pa.clone().unwrap();
    let all = [
        "numbers.search",
        "numbers.read",
        "numbers.manage",
        "numbers.assign",
        "numbers.kyc.read",
    ];
    let alice = pa.customer("SANDBOX", &all, &all).await;
    let bob = pa.customer("SANDBOX", &all, &all).await;
    let reader = pa
        .customer(
            "SANDBOX",
            &all,
            &["numbers.read", "numbers.search", "numbers.kyc.read"],
        )
        .await;
    let admin = pa
        .customer_as(
            "SANDBOX",
            "FIRST_PARTY",
            &["numbers.admin"],
            &["numbers.admin"],
        )
        .await;
    let s = OwnerScope {
        tenant_id: alice.tenant_id,
        application_id: alice.application_id,
        environment_id: alice.environment_id,
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: alice.service_account_id,
    };
    // A provisioned, route-ready number of Alice's.
    let page = h
        .customer_with(
            &alice.secret,
            Method::GET,
            "/v1/numbers/search?countryCode=US&numberType=GEOGRAPHIC&region=US-CA",
            None,
            &[],
        )
        .await;
    let number =
        Uuid::parse_str(page.body["data"][0]["numberResourceId"].as_str().unwrap()).unwrap();
    let caps = [C::VoiceInbound];
    let decide = |class: &str, subject_kind: &str, subject: Uuid| {
        json!({
            "subjectKind": subject_kind, "subjectRef": subject, "countryCode": "US",
            "numberType": "GEOGRAPHIC", "ownershipClass": class,
            "capabilities": ["VOICE_INBOUND"], "numberResourceId": number,
        })
    };
    let elig = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/eligibility",
            Some(decide("ORGANIZATION", "ORGANIZATION", alice.tenant_id)),
            &[],
        )
        .await;
    let res = a
        .create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: number,
                capabilities: caps.to_vec(),
                hold_seconds: Some(3_600),
                eligibility_decision_id: Some(
                    Uuid::parse_str(elig.body["eligibilityDecisionId"].as_str().unwrap()).unwrap(),
                ),
            },
            "api-j-res-0001",
        )
        .await
        .unwrap()
        .reservation;
    ax::drain(&runner).await;
    a.provision(
        &ctx(),
        &s,
        standing(),
        ProvisionRequest {
            reservation_id: res.id,
            capabilities: caps.to_vec(),
            eligibility_decision_id: None,
        },
        "api-j-acq-0001",
    )
    .await
    .unwrap();
    ax::drain(&runner).await;
    let acq = h
        .numbers
        .acquisition_of_reservation(res.id)
        .await
        .unwrap()
        .unwrap();
    a.configure_routes(&ctx(), &s, acq.id, &caps, "api-j-route-001")
        .await
        .unwrap();
    ax::drain(&runner).await;
    let route = h
        .numbers
        .current_routes(number)
        .await
        .unwrap()
        .pop()
        .unwrap();
    h.internal_json(
        &h.peers.telephony,
        Method::POST,
        &format!("/internal/v1/number-resources/{number}/route-readiness"),
        json!({ "family": "VOICE", "routeRef": route.route_ref, "routeRevision": route.route_revision }),
    )
    .await;

    // A real Platform Access principal of Alice's environment owns a personal number.
    let owner = pa.principal(alice.environment_id).await;
    let foreign = pa.principal(bob.environment_id).await;
    let personal = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/eligibility",
            Some(decide("PERSONAL", "INDIVIDUAL", owner)),
            &[],
        )
        .await;
    let body = |principal: Uuid| {
        json!({
            "acquisitionId": acq.id,
            "ownershipClass": "PERSONAL",
            "ownerPrincipalId": principal,
            "routing": { "voice": { "kind": "PRINCIPAL", "ref": principal }, "sms": { "kind": "NONE", "ref": null } },
            "capabilities": ["VOICE_INBOUND"],
            "eligibilityDecisionId": personal.body["eligibilityDecisionId"],
        })
    };
    let post = |token: &str, path: &str, b: serde_json::Value, key: Option<&str>| {
        let token = token.to_string();
        let path = path.to_string();
        let key = key.map(str::to_string);
        let h = &h;
        async move {
            let headers: Vec<(&str, &str)> = key
                .as_deref()
                .map(|k| vec![("idempotency-key", k)])
                .unwrap_or_default();
            h.customer_with(&token, Method::POST, &path, Some(b), &headers)
                .await
        }
    };
    // Scope: numbers.read cannot assign; the key is required.
    let r = post(
        &reader.secret,
        "/v1/numbers/assignments",
        body(owner),
        Some("api-j-asg-0001"),
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE")
    );
    let r = post(&alice.secret, "/v1/numbers/assignments", body(owner), None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // A principal of another environment is not found in Alice's.
    let r = post(
        &alice.secret,
        "/v1/numbers/assignments",
        body(foreign),
        Some("api-j-asg-0002"),
    )
    .await;
    assert_eq!(
        r.body["error"]["reason"], "PRINCIPAL_NOT_FOUND",
        "{:?}",
        r.body
    );
    // Bob cannot assign Alice's acquisition.
    let r = post(
        &bob.secret,
        "/v1/numbers/assignments",
        body(owner),
        Some("api-j-asg-0003"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let created = post(
        &alice.secret,
        "/v1/numbers/assignments",
        body(owner),
        Some("api-j-asg-0004"),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    conforms(&format!("{COMMON}DcnAssignment"), &created.body);
    assert_eq!(created.body["ownership"]["ownerRef"], owner.to_string());
    let replay = post(
        &alice.secret,
        "/v1/numbers/assignments",
        body(owner),
        Some("api-j-asg-0004"),
    )
    .await;
    assert_eq!(
        (
            replay.status,
            replay.headers["idempotent-replayed"].to_str().unwrap()
        ),
        (StatusCode::OK, "true")
    );
    let aid = created.body["assignmentId"].as_str().unwrap().to_string();
    // Object-level checks: Bob sees nothing of Alice's assignment.
    for (method, path) in [
        (Method::GET, format!("/v1/numbers/assignments/{aid}")),
        (
            Method::GET,
            format!("/v1/numbers/assignments/{aid}/routing"),
        ),
        (
            Method::POST,
            format!("/v1/numbers/assignments/{aid}/activate"),
        ),
    ] {
        let r = h.customer_with(&bob.secret, method, &path, None, &[]).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
    }
    let r = post(
        &reader.secret,
        &format!("/v1/numbers/assignments/{aid}/activate"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let activated = h
        .customer_with(
            &alice.secret,
            Method::POST,
            &format!("/v1/numbers/assignments/{aid}/activate"),
            None,
            &[],
        )
        .await;
    assert_eq!(activated.body["status"], "ACTIVE", "{:?}", activated.body);
    let routing = h
        .customer_with(
            &alice.secret,
            Method::GET,
            &format!("/v1/numbers/assignments/{aid}/routing"),
            None,
            &[],
        )
        .await;
    conforms(&format!("{COMMON}AssignmentRouting"), &routing.body);
    assert_eq!(routing.body["readiness"][0]["ready"], true);
    // Routing changes need numbers.manage and the current revision.
    let change = json!({
        "expectedRoutingRevision": 1,
        "routing": { "voice": { "kind": "VOICEMAIL", "ref": Uuid::new_v4() }, "sms": { "kind": "NONE", "ref": null } },
    });
    let r = post(
        &alice.secret,
        &format!("/v1/numbers/assignments/{aid}/routing"),
        change.clone(),
        None,
    )
    .await;
    assert_eq!(r.body["routing"]["revision"], 2, "{:?}", r.body);
    let r = post(
        &alice.secret,
        &format!("/v1/numbers/assignments/{aid}/routing"),
        change,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // A cross-owner transfer is refused through the API too.
    let r = post(
        &alice.secret,
        &format!("/v1/numbers/assignments/{aid}/transfer"),
        json!({
            "ownershipClass": "ORGANIZATION",
            "routing": { "voice": { "kind": "NONE", "ref": null }, "sms": { "kind": "NONE", "ref": null } },
            "expectedRoutingRevision": 2,
        }),
        Some("api-j-xfer-001"),
    )
    .await;
    assert_eq!(
        r.body["error"]["reason"],
        "CROSS_OWNER_TRANSFER_UNSUPPORTED"
    );
    // Listing filters and pages; Bob's list is empty.
    let list = h
        .customer_with(
            &alice.secret,
            Method::GET,
            "/v1/numbers/assignments?status=ACTIVE&ownershipClass=PERSONAL&limit=1",
            None,
            &[],
        )
        .await;
    conforms(&format!("{COMMON}DcnAssignmentList"), &list.body);
    assert_eq!(list.body["data"].as_array().unwrap().len(), 1);
    let r = h
        .customer_with(
            &alice.secret,
            Method::GET,
            "/v1/numbers/assignments?status=BOGUS",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let bobs = h
        .customer_with(
            &bob.secret,
            Method::GET,
            "/v1/numbers/assignments",
            None,
            &[],
        )
        .await;
    assert!(bobs.body["data"].as_array().unwrap().is_empty());
    // Pools.
    let pool = post(
        &alice.secret,
        "/v1/numbers/pools",
        json!({ "name": "API pool" }),
        None,
    )
    .await;
    assert_eq!(pool.status, StatusCode::CREATED);
    conforms(&format!("{COMMON}NumberPool"), &pool.body);
    let pid = pool.body["poolId"].as_str().unwrap();
    let r = post(
        &alice.secret,
        &format!("/v1/numbers/pools/{pid}/members"),
        json!({ "assignmentId": aid }),
        None,
    )
    .await;
    assert_eq!(r.body["error"]["reason"], "POOL_MEMBER_NOT_ORGANIZATIONAL");
    let got = h
        .customer_with(
            &alice.secret,
            Method::GET,
            &format!("/v1/numbers/pools/{pid}"),
            None,
            &[],
        )
        .await;
    conforms(&format!("{COMMON}NumberPool"), &got.body);
    assert_eq!(
        h.customer_with(
            &bob.secret,
            Method::GET,
            &format!("/v1/numbers/pools/{pid}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Administration: every owner's history of the number, operator-attributed only.
    let path = format!("/v1/admin/number-resources/{number}/assignment-history");
    assert_eq!(
        h.customer_with(&alice.secret, Method::GET, &path, None, &[])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let r = h
        .customer_json(&admin.secret, Method::GET, &path, None, None)
        .await;
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
    let r = h
        .customer_json(&admin.secret, Method::GET, &path, None, Some("ops.alice"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}AssignmentHistoryList"), &r.body);
    assert!(r.body["data"].as_array().unwrap().len() >= 6);
    // Provider identifiers never appear in any of it.
    for v in [&created.body, &routing.body, &list.body, &got.body, &r.body] {
        let text = v.to_string();
        assert!(
            !text.contains("syn-") && !text.contains("externalRef") && !text.contains("routeRef")
        );
    }
    let served = h
        .raw(
            axum::http::Request::get("/openapi.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    for path in [
        "/v1/numbers/assignments",
        "/v1/numbers/assignments/{assignmentId}/activate",
        "/v1/numbers/assignments/{assignmentId}/routing",
        "/v1/numbers/assignments/{assignmentId}/offboarding",
        "/v1/numbers/assignments/{assignmentId}/transfer",
        "/v1/numbers/pools",
        "/v1/numbers/pools/{poolId}",
        "/v1/numbers/pools/{poolId}/members",
        "/v1/numbers/pools/{poolId}/members/{assignmentId}",
        "/v1/admin/number-resources/{numberResourceId}/assignment-history",
    ] {
        assert!(served.body["paths"].get(path).is_some(), "{path}");
    }
}
