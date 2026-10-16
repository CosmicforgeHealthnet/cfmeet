//! Part IV Workstream I: provisioning and activation prerequisites — reserve → provision
//! through G's durable executor, eligibility / KYC re-evaluation, hold read-back,
//! idempotency and concurrency, unknown / lost / partial / interrupted outcomes, delayed
//! callbacks, reservation-expiry races, provider route configuration with stale /
//! changed-binding protection, consumer readiness through the authenticated internal
//! contract, the customer API, database guards and migration rollback.
mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::{
    acquisition as ax, catalog, contracts::conforms, exec, kyc as fx, providers as px, scalar,
    scope, strings, Harness,
};
use numbering::{
    acquisition::{
        provisioning::ProvisionRequest,
        reservations::{Expiry, ReservationRequest},
        search::{SearchOutcome, SearchRequest},
        Acquisition, AcquisitionSettings,
    },
    domain::RequestContext,
    numbers::{
        acquisitions::RouteFamily,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::{
            evidence::EvidenceBytes, verifier::DeterministicKycVerifier, EvidenceKind, NewCase,
            SubjectKind, TenantClassification,
        },
        Capability as C, DomainError, EnvironmentKind, LifecycleState as S, NumberType, OwnerScope,
        OwnershipClass,
    },
    providers::{
        commands::CommandStatus, deterministic::Fault, reconcile::Reconciled,
        ProviderOperation as Op,
    },
};
use serde_json::json;
use uuid::Uuid;

const COMMON: &str = "numbering/common/v1.schema.json#/$defs/";

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

struct I {
    h: Harness,
    p: px::Providers,
    a: Acquisition,
    kyc: numbering::numbers::kyc::KycService,
    runner: numbering::infra::jobs::JobRunner,
}

async fn setup() -> Option<I> {
    let h = Harness::new().await?;
    let (_, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(DeterministicKycVerifier::new()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let a = ax::of(&h, &p, AcquisitionSettings::default());
    let runner = ax::runner(&h, &p, &a).with_handler(Arc::new(
        numbering::numbers::kyc::VerificationHandler { kyc: kyc.clone() },
    ));
    Some(I {
        h,
        p,
        a,
        kyc,
        runner,
    })
}

fn sandbox() -> OwnerScope {
    scope(EnvironmentKind::Sandbox)
}

impl I {
    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    async fn reconcile(&self) {
        self.h.clock.advance(Duration::seconds(31));
        self.drain().await;
    }

    async fn ingest(&self, s: &OwnerScope) {
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
    }

    async fn id_of(&self, external_ref: &str) -> Uuid {
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

    async fn state(&self, id: Uuid) -> S {
        self.h.numbers.resource(id).await.unwrap().lifecycle_state()
    }

    async fn decide(
        &self,
        s: &OwnerScope,
        subject: (SubjectKind, Uuid),
        number_type: NumberType,
        ownership: OwnershipClass,
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
                    ownership_class: ownership,
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

    /// A geographic number reserved (HELD) on an ELIGIBLE organization decision.
    async fn held_geo(&self, s: &OwnerScope, external_ref: &str, caps: &[C]) -> (Uuid, Uuid, Uuid) {
        self.ingest(s).await;
        let id = self.id_of(external_ref).await;
        let decision = self
            .decide(
                s,
                (SubjectKind::Organization, Uuid::new_v4()),
                NumberType::Geographic,
                OwnershipClass::Organization,
                caps,
                id,
            )
            .await;
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
                &format!("rsv-{}", Uuid::new_v4().simple()),
            )
            .await
            .unwrap()
            .reservation;
        self.drain().await;
        assert_eq!(
            self.h.numbers.reservation(s, r.id).await.unwrap().status,
            "HELD"
        );
        (id, r.id, decision)
    }

    async fn provision(
        &self,
        s: &OwnerScope,
        reservation: Uuid,
        caps: &[C],
        key: &str,
    ) -> Result<numbering::acquisition::provisioning::Provisioned, DomainError> {
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
                key,
            )
            .await
    }

    async fn acquisition(
        &self,
        s: &OwnerScope,
        id: Uuid,
    ) -> numbering::numbers::acquisitions::AcquisitionRow {
        self.h.numbers.acquisition(s, id).await.unwrap()
    }

    async fn bindings(&self, id: Uuid) -> usize {
        self.h
            .numbers
            .bindings(id)
            .await
            .unwrap()
            .iter()
            .filter(|b| b.status == "ACTIVE")
            .count()
    }

    /// A PROVISIONED voice + inbound-SMS geographic number.
    async fn provisioned(&self, s: &OwnerScope) -> (Uuid, Uuid) {
        let caps = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];
        let (id, rid, _) = self.held_geo(s, "syn-voice-sms-geo", &caps).await;
        let acq = self
            .provision(s, rid, &caps, &format!("acq-{}", Uuid::new_v4().simple()))
            .await
            .unwrap()
            .acquisition;
        self.drain().await;
        assert_eq!(self.acquisition(s, acq.id).await.status, "PROVISIONED");
        (id, acq.id)
    }
}

const GEO: [C; 3] = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];

// ---------------------------------------------------------------------------------------
// Step 6: provisioning orchestration
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_held_reservation_provisions_exactly_one_resource_and_binding_never_an_active_number() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    let p = t.provision(&s, rid, &GEO, "acq-key-0001").await.unwrap();
    assert!(!p.replayed);
    let acq = p.acquisition;
    assert_eq!(acq.status, "PROVISIONING");
    assert_eq!(t.state(id).await, S::Provisioning);
    // Eligibility was re-evaluated (a NEW decision is the basis), never just reused.
    let d =
        t.h.numbers
            .eligibility_decision(&s, acq.eligibility_decision_ref)
            .await
            .unwrap();
    assert_eq!(
        (d.outcome.as_str(), d.number_resource_id),
        ("ELIGIBLE", Some(id))
    );
    // Duplicate requests: the same key replays, another key is refused.
    let again = t.provision(&s, rid, &GEO, "acq-key-0001").await.unwrap();
    assert!(again.replayed);
    assert_eq!(again.acquisition.id, acq.id);
    assert_eq!(
        t.provision(&s, rid, &GEO, "acq-key-0002")
            .await
            .unwrap_err()
            .code(),
        "ACQUISITION_EXISTS"
    );
    t.drain().await;
    let acq = t.acquisition(&s, acq.id).await;
    assert_eq!(acq.status, "PROVISIONED");
    assert!(acq.binding_id.is_some());
    assert_eq!(t.state(id).await, S::Provisioned);
    assert_eq!(t.bindings(id).await, 1);
    let cmd = acq.provision_command_id.unwrap();
    assert_eq!(t.p.sim.executions(Op::Provision, cmd), 1);
    // The reservation is CONVERTED into the acquisition (a later expiry cannot release it).
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(
        (r.status.as_str(), r.converted_to.as_deref()),
        ("CONVERTED", Some("ACQUISITION"))
    );
    assert_eq!(
        t.provision(&s, rid, &GEO, "acq-key-0003")
            .await
            .unwrap_err()
            .code(),
        "HOLD_REQUIRED"
    );
    // A provisioned number is not an active customer number: no assignment, not ACTIVE,
    // no route ready.
    assert_eq!(t.h.scalar("SELECT count(*) FROM dcn_assignments").await, 0);
    let view = t.a.acquisition_status(&s, acq.id).await.unwrap();
    assert!(!view.activation_ready());
    assert!(view
        .routes
        .iter()
        .all(|r| r.reason == Some("ROUTE_NOT_CONFIGURED")));
    assert!(!view.live_routing);
    assert_eq!(
        view.served_capabilities,
        vec!["SMS_INBOUND", "VOICE_INBOUND", "VOICE_OUTBOUND"]
    );
    // Canonical identity is unchanged; the outbox has the acquisition lifecycle.
    assert_eq!(t.h.numbers.resource(id).await.unwrap().id, id);
    assert_eq!(
        strings(
            t.h.conn(),
            &format!("SELECT payload->>'status' FROM numbering_outbox_events WHERE event_type = 'number.acquisition.changed' \
                       AND aggregate_id = '{}' ORDER BY id", acq.id),
        )
        .await
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>(),
        ["PROVISIONED".to_string(), "PROVISIONING".to_string()].into_iter().collect()
    );
}

#[tokio::test]
async fn provisioning_needs_a_live_reservation_a_fresh_eligibility_and_supported_capabilities() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    t.ingest(&s).await;
    // A pre-eligibility hold never provisions.
    let id = t.id_of("syn-voice-only").await;
    let pre =
        t.a.create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: id,
                capabilities: vec![],
                hold_seconds: None,
                eligibility_decision_id: None,
            },
            "pre-hold-0001",
        )
        .await
        .unwrap()
        .reservation;
    t.drain().await;
    assert_eq!(
        t.provision(&s, pre.id, &[C::VoiceInbound], "prov-pre-01")
            .await
            .unwrap_err()
            .code(),
        "ELIGIBILITY_REQUIRED"
    );
    // ...unless a decision is presented at provisioning (re-evaluated, for this number).
    let decision = t
        .decide(
            &s,
            (SubjectKind::Organization, Uuid::new_v4()),
            NumberType::Geographic,
            OwnershipClass::Organization,
            &[C::VoiceInbound],
            id,
        )
        .await;
    let ok =
        t.a.provision(
            &ctx(),
            &s,
            standing(),
            ProvisionRequest {
                reservation_id: pre.id,
                capabilities: vec![C::VoiceInbound],
                eligibility_decision_id: Some(decision),
            },
            "prov-pre-02",
        )
        .await
        .unwrap();
    assert_eq!(ok.acquisition.status, "PROVISIONING");
    // Another scope cannot provision (or see) it; unsupported capabilities are refused.
    let (_, rid, _) = t.held_geo(&s, "syn-inbound-only", &[C::VoiceInbound]).await;
    assert_eq!(
        t.provision(&sandbox(), rid, &[C::VoiceInbound], "prov-x-0001")
            .await
            .unwrap_err()
            .code(),
        "NOT_FOUND"
    );
    assert_eq!(
        t.provision(&s, rid, &[C::SmsOutbound], "prov-x-0002")
            .await
            .unwrap_err()
            .code(),
        "CAPABILITY_NOT_SUPPORTED"
    );
    assert_eq!(
        t.provision(&s, rid, &[], "prov-x-0003")
            .await
            .unwrap_err()
            .code(),
        "VALIDATION_ERROR"
    );
    // A cancelled reservation, and a reservation that expired, never provision.
    t.a.cancel_reservation(&ctx(), &s, rid).await.unwrap();
    assert_eq!(
        t.provision(&s, rid, &[C::VoiceInbound], "prov-x-0004")
            .await
            .unwrap_err()
            .code(),
        "HOLD_REQUIRED"
    );
    let (_, rid2, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    t.h.clock.advance(Duration::hours(2));
    assert_eq!(
        t.provision(&s, rid2, &GEO, "prov-x-0005")
            .await
            .unwrap_err()
            .code(),
        "RESERVATION_EXPIRED"
    );
}

#[tokio::test]
async fn a_kyc_revocation_before_provisioning_is_never_bypassed() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    t.ingest(&s).await;
    let id = t.id_of("syn-voice-sms-mobile").await;
    let principal = Uuid::new_v4();
    // An approved personal KYC case (deterministic verifier) and an ELIGIBLE decision.
    let case = t
        .kyc
        .create_case(
            &ctx(),
            &s,
            NewCase {
                subject_kind: SubjectKind::Individual,
                subject_ref: principal,
                country: "US".into(),
                number_type: NumberType::Mobile,
                ownership_class: OwnershipClass::Personal,
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
            &s,
            case.id,
            EvidenceKind::Passport,
            "application/pdf",
            EvidenceBytes::new(fx::pdf("passport")),
            None,
        )
        .await
        .unwrap();
    let c = t.kyc.case(&s, case.id).await.unwrap();
    t.kyc
        .submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap();
    t.drain().await;
    assert_eq!(t.kyc.case(&s, case.id).await.unwrap().status, "APPROVED");
    let caps = [C::VoiceInbound, C::SmsOutbound];
    let decision = t
        .decide(
            &s,
            (SubjectKind::Individual, principal),
            NumberType::Mobile,
            OwnershipClass::Personal,
            &caps,
            id,
        )
        .await;
    let r =
        t.a.create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: id,
                capabilities: caps.to_vec(),
                hold_seconds: None,
                eligibility_decision_id: Some(decision),
            },
            "kyc-rsv-0001",
        )
        .await
        .unwrap()
        .reservation;
    t.drain().await;
    // The approval is revoked after the hold: the recorded decision is still "valid",
    // but provisioning re-evaluates and refuses.
    t.kyc
        .revoke_case(&ctx(), &fx::operator(), case.id, "FRAUD_SIGNAL")
        .await
        .unwrap();
    let err = t
        .provision(&s, r.id, &caps, "kyc-prov-0001")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "ELIGIBILITY_REQUIRED");
    assert!(
        err.to_string().contains("KYC_REQUIRED") || err.to_string().contains("DENIED"),
        "{err}"
    );
    assert_eq!(t.state(id).await, S::Reserved);
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_acquisitions").await,
        0
    );
    assert_eq!(
        t.p.sim
            .calls()
            .iter()
            .filter(|(op, _, _)| *op == Op::Provision)
            .count(),
        0
    );
}

#[tokio::test]
async fn a_lost_provider_hold_is_detected_by_read_back_before_provisioning() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    t.ingest(&s).await;
    let id = t.id_of("syn-voice-only").await;
    let decision = t
        .decide(
            &s,
            (SubjectKind::Organization, Uuid::new_v4()),
            NumberType::Geographic,
            OwnershipClass::Organization,
            &[C::VoiceInbound],
            id,
        )
        .await;
    // A 2-hour reservation, while the provider holds for at most 30 minutes.
    let r =
        t.a.create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: id,
                capabilities: vec![C::VoiceInbound],
                hold_seconds: Some(7_200),
                eligibility_decision_id: Some(decision),
            },
            "lost-hold-0001",
        )
        .await
        .unwrap()
        .reservation;
    t.drain().await;
    t.h.clock.advance(Duration::minutes(31));
    t.p.sim.expire_holds();
    let err =
        t.a.provision(
            &ctx(),
            &s,
            standing(),
            ProvisionRequest {
                reservation_id: r.id,
                capabilities: vec![C::VoiceInbound],
                eligibility_decision_id: Some(decision),
            },
            "lost-hold-prov",
        )
        .await
        .unwrap_err();
    // Eligibility is re-evaluated (still ELIGIBLE); the hold read-back refuses.
    assert_eq!(err.code(), "HOLD_LOST");
    assert!(
        t.h.scalar(
            "SELECT count(*) FROM numbering_incidents WHERE category = 'PROVIDER_STATE_DRIFT'"
        )
        .await
            == 1
    );
    assert_eq!(t.state(id).await, S::Reserved);
    assert_eq!(
        t.p.sim
            .calls()
            .iter()
            .filter(|(op, _, _)| *op == Op::Provision)
            .count(),
        0
    );
}

#[tokio::test]
async fn concurrent_provisioning_requests_acquire_exactly_once() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    let tasks: Vec<_> = (0..5)
        .map(|i| {
            let a = t.a.clone();
            tokio::spawn(async move {
                a.provision(
                    &RequestContext::generate(),
                    &s,
                    standing(),
                    ProvisionRequest {
                        reservation_id: rid,
                        capabilities: GEO.to_vec(),
                        eligibility_decision_id: None,
                    },
                    &format!("race-prov-{i:04}"),
                )
                .await
                .map(|p| p.acquisition.id)
                .map_err(|e| e.code().to_string())
            })
        })
        .collect();
    let mut won = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => won += 1,
            Err(code) => assert!(
                [
                    "ACQUISITION_EXISTS",
                    "VERSION_CONFLICT",
                    "INVALID_TRANSITION"
                ]
                .contains(&code.as_str()),
                "{code}"
            ),
        }
    }
    assert_eq!(won, 1);
    t.drain().await;
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_acquisitions").await,
        1
    );
    assert_eq!(t.bindings(id).await, 1);
    assert_eq!(
        t.p.sim
            .calls()
            .iter()
            .filter(|(op, _, o)| *op == Op::Provision && *o == "EXECUTED")
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------------------
// Step 8: uncertain outcomes and recovery
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_provisioning_is_reconciled_from_read_back_never_resent() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    t.p.sim.inject(Op::Provision, Fault::TimeoutAfterExecution);
    let acq = t
        .provision(&s, rid, &GEO, "unk-prov-0001")
        .await
        .unwrap()
        .acquisition;
    t.drain().await;
    let a = t.acquisition(&s, acq.id).await;
    assert_eq!(a.status, "UNKNOWN");
    assert_eq!(t.state(id).await, S::UnknownProviderState);
    assert_eq!(t.bindings(id).await, 0);
    // The reservation still holds (not converted, not expirable under the provider).
    assert_eq!(
        t.h.numbers.reservation(&s, rid).await.unwrap().status,
        "HELD"
    );
    // A delayed provider callback (the provisioning did happen) is ingested, de-duplicated
    // and leads to reconciliation — never to a second acquisition.
    for cb in t.p.sim.take_callbacks() {
        t.p.commands
            .ingest_callback(&ctx(), "synthetic", &cb)
            .await
            .unwrap();
        t.p.commands
            .ingest_callback(&ctx(), "synthetic", &cb)
            .await
            .unwrap();
    }
    t.reconcile().await;
    let a = t.acquisition(&s, acq.id).await;
    assert_eq!(a.status, "PROVISIONED");
    assert_eq!(t.state(id).await, S::Provisioned);
    assert_eq!(t.bindings(id).await, 1);
    assert_eq!(
        t.p.sim
            .executions(Op::Provision, a.provision_command_id.unwrap()),
        1
    );
    let cmd =
        t.p.commands
            .command(a.provision_command_id.unwrap())
            .await
            .unwrap();
    assert_eq!(cmd.status(), CommandStatus::ReconciledSucceeded);
    assert_eq!(
        t.h.numbers
            .reservation(&s, rid)
            .await
            .unwrap()
            .converted_to
            .as_deref(),
        Some("ACQUISITION")
    );
}

#[tokio::test]
async fn a_lost_request_fails_the_acquisition_and_a_new_attempt_succeeds_once() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    t.p.sim.inject(Op::Provision, Fault::LostRequest);
    let acq = t
        .provision(&s, rid, &GEO, "lost-prov-001")
        .await
        .unwrap()
        .acquisition;
    t.drain().await;
    assert_eq!(t.acquisition(&s, acq.id).await.status, "UNKNOWN");
    t.reconcile().await;
    let failed = t.acquisition(&s, acq.id).await;
    assert_eq!(failed.status, "FAILED");
    assert_eq!(t.state(id).await, S::Reserved);
    assert_eq!(
        t.h.numbers.reservation(&s, rid).await.unwrap().status,
        "HELD"
    );
    // A definite failure may be retried (a new attempt, a new command).
    let retry = t.provision(&s, rid, &GEO, "lost-prov-002").await.unwrap();
    assert_eq!(retry.acquisition.id, acq.id);
    assert_ne!(
        retry.acquisition.provision_command_id,
        failed.provision_command_id
    );
    t.drain().await;
    assert_eq!(t.acquisition(&s, acq.id).await.status, "PROVISIONED");
    assert_eq!(t.bindings(id).await, 1);
}

#[tokio::test]
async fn partial_provisioning_binds_only_what_is_served_and_raises_drift() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    t.p.sim.inject(Op::Provision, Fault::PartialProvision);
    let acq = t
        .provision(&s, rid, &GEO, "part-prov-001")
        .await
        .unwrap()
        .acquisition;
    t.drain().await;
    t.reconcile().await;
    let a = t.acquisition(&s, acq.id).await;
    assert_eq!(a.status, "PROVISIONED");
    let bound = t.h.numbers.bindings(id).await.unwrap();
    assert_eq!(bound.len(), 1);
    assert_eq!(
        bound[0].capabilities.len(),
        1,
        "{:?}",
        bound[0].capabilities
    );
    assert!(
        t.h.scalar("SELECT count(*) FROM numbering_incidents WHERE category = 'PROVIDER_STATE_DRIFT' AND status = 'OPEN'")
            .await
            >= 1
    );
    // Routes for a capability the provider does not serve are refused.
    let view = t.a.acquisition_status(&s, acq.id).await.unwrap();
    let unserved = GEO
        .iter()
        .find(|c| !view.served_capabilities.contains(&c.as_str().to_string()))
        .copied()
        .unwrap();
    assert_eq!(
        t.a.configure_routes(&ctx(), &s, acq.id, &[unserved], "part-route-01")
            .await
            .unwrap_err()
            .code(),
        "CAPABILITY_NOT_SUPPORTED"
    );
}

#[tokio::test]
async fn a_process_interruption_mid_provisioning_is_recovered_by_reconciliation() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    let acq = t
        .provision(&s, rid, &GEO, "crash-prov-01")
        .await
        .unwrap()
        .acquisition;
    let cmd = acq.provision_command_id.unwrap();
    // The worker claims the dispatch, commits DISPATCHING, then dies.
    let doomed = px::runner(&t.h, &t.p.commands, "doomed");
    assert_eq!(doomed.claim(Uuid::new_v4()).await.unwrap().len(), 1);
    exec(
        t.h.conn(),
        &format!("UPDATE number_provider_commands SET status = 'DISPATCHING', attempts = 1, revision = revision + 1 WHERE id = '{cmd}'"),
    )
    .await
    .unwrap();
    t.h.clock.advance(Duration::seconds(31));
    assert_eq!(t.runner.run_due().await.unwrap().parked_unknown, 1);
    assert_eq!(
        t.p.commands.recover_interrupted(&ctx(), 10).await.unwrap(),
        1
    );
    assert_eq!(t.acquisition(&s, acq.id).await.status, "UNKNOWN");
    assert_eq!(t.state(id).await, S::UnknownProviderState);
    assert_eq!(
        t.p.commands.reconcile_command(&ctx(), cmd).await.unwrap(),
        Reconciled::DidNotHappen
    );
    assert_eq!(t.acquisition(&s, acq.id).await.status, "FAILED");
    assert_eq!(t.p.sim.executions(Op::Provision, cmd), 0);
    assert_eq!(t.state(id).await, S::Reserved);
}

#[tokio::test]
async fn reservation_expiry_racing_provisioning_never_releases_the_committed_acquisition() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, rid, _) = t.held_geo(&s, "syn-voice-sms-geo", &GEO).await;
    let acq = t
        .provision(&s, rid, &GEO, "race-exp-0001")
        .await
        .unwrap()
        .acquisition;
    // The reservation expires while the provisioning is in flight.
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    t.h.clock.set(r.expires_at + Duration::seconds(1));
    assert_eq!(
        t.a.expire_reservation(&ctx(), rid).await.unwrap(),
        Expiry::Deferred
    );
    t.a.sweep_reservations(&ctx(), 100).await.unwrap();
    t.drain().await;
    t.h.clock.advance(Duration::minutes(5));
    t.drain().await;
    assert_eq!(t.acquisition(&s, acq.id).await.status, "PROVISIONED");
    let r = t.h.numbers.reservation(&s, rid).await.unwrap();
    assert_eq!(
        (r.status.as_str(), r.converted_to.as_deref()),
        ("CONVERTED", Some("ACQUISITION"))
    );
    assert_eq!(t.state(id).await, S::Provisioned);
    assert!(t
        .p
        .commands
        .reservation_commands(rid)
        .await
        .unwrap()
        .iter()
        .all(|c| c.operation != "RELEASE_HOLD"));
    assert_eq!(
        t.a.expire_reservation(&ctx(), rid).await.unwrap(),
        Expiry::NotCurrent
    );
}

// ---------------------------------------------------------------------------------------
// Step 7: capability configuration and activation readiness
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn routes_become_ready_only_on_provider_evidence_and_their_consumers_acknowledgement() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.provisioned(&s).await;
    t.a.configure_routes(
        &ctx(),
        &s,
        acq,
        &[C::VoiceInbound, C::SmsInbound],
        "route-key-001",
    )
    .await
    .unwrap();
    // Requested, not configured: the provider has not answered.
    let routes = t.h.numbers.current_routes(id).await.unwrap();
    assert_eq!(routes.len(), 2);
    assert!(routes.iter().all(|r| r.status == "REQUESTED"));
    // Replays never resubmit.
    let (_, replayed) =
        t.a.configure_routes(
            &ctx(),
            &s,
            acq,
            &[C::VoiceInbound, C::SmsInbound],
            "route-key-001",
        )
        .await
        .unwrap();
    assert!(replayed);
    t.drain().await;
    let routes = t.h.numbers.current_routes(id).await.unwrap();
    assert!(routes
        .iter()
        .all(|r| r.status == "CONFIGURED" && r.evidence_ref.is_some()));
    let resource = t.h.numbers.resource(id).await.unwrap();
    assert!(resource.voice_route_ref.is_some() && resource.sms_route_ref.is_some());
    let view = t.a.acquisition_status(&s, acq).await.unwrap();
    assert!(view
        .routes
        .iter()
        .all(|r| r.reason == Some("CONSUMER_NOT_READY")));
    assert!(!view.activation_ready());
    // Consumers acknowledge through the authenticated internal contract.
    let voice = routes.iter().find(|r| r.family == "VOICE").unwrap();
    let sms = routes.iter().find(|r| r.family == "SMS").unwrap();
    let path = format!("/internal/v1/number-resources/{id}/route-readiness");
    let body = |r: &numbering::numbers::acquisitions::RouteConfigurationRow, revision: i64| json!({ "family": r.family, "routeRef": r.route_ref, "routeRevision": revision });
    // Calling is not a route consumer (no permission); Telephony never confirms SMS.
    let r =
        t.h.internal_json(&t.h.peers.calling, Method::POST, &path, body(voice, 1))
            .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    let r =
        t.h.internal_json(&t.h.peers.telephony, Method::POST, &path, body(sms, 1))
            .await;
    assert_eq!(r.body["error"]["reason"], "CONSUMER_MISMATCH");
    let r =
        t.h.internal_json(&t.h.peers.telephony, Method::POST, &path, body(voice, 7))
            .await;
    assert_eq!(r.body["error"]["reason"], "STALE_ROUTE_REVISION");
    let r =
        t.h.internal_json(&t.h.peers.telephony, Method::POST, &path, body(voice, 1))
            .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}RouteReadiness"), &r.body);
    let again =
        t.h.internal_json(&t.h.peers.telephony, Method::POST, &path, body(voice, 1))
            .await;
    assert_eq!(again.body["acknowledgedAt"], r.body["acknowledgedAt"]);
    let r =
        t.h.internal_json(&t.h.peers.sms_gateway, Method::POST, &path, body(sms, 1))
            .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let view = t.a.acquisition_status(&s, acq).await.unwrap();
    assert!(view.activation_ready(), "{:?}", view.routes);
    assert!(!view.live_routing);
    // The internal route view (consumers read route references there).
    let routes_view =
        t.h.internal(
            &t.h.peers.telephony,
            Method::GET,
            &format!("/internal/v1/number-resources/{id}/routes"),
        )
        .await;
    assert_eq!(routes_view.status, StatusCode::OK);
    conforms(&format!("{COMMON}NumberRoutes"), &routes_view.body);
    assert_eq!(routes_view.body["synthetic"], true);
    // Still not ACTIVE: activation is an assignment (Workstream J).
    assert_eq!(t.state(id).await, S::Provisioned);
}

#[tokio::test]
async fn a_binding_change_or_an_uncertain_configuration_never_leaves_a_stale_route_ready() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.provisioned(&s).await;
    // An unknown configuration outcome is read back, never assumed.
    t.p.sim
        .inject(Op::ConfigureVoice, Fault::TimeoutAfterExecution);
    t.a.configure_routes(&ctx(), &s, acq, &[C::VoiceInbound], "route-unk-01")
        .await
        .unwrap();
    t.drain().await;
    assert_eq!(
        t.h.numbers.current_routes(id).await.unwrap()[0].status,
        "UNKNOWN"
    );
    // While in flight, another request does not stack a second configuration.
    t.a.configure_routes(&ctx(), &s, acq, &[C::VoiceInbound], "route-unk-02")
        .await
        .unwrap();
    assert_eq!(t.h.numbers.route_history(id).await.unwrap().len(), 1);
    t.reconcile().await;
    let voice = t.h.numbers.current_routes(id).await.unwrap()[0].clone();
    assert_eq!(
        (voice.status.as_str(), voice.route_revision),
        ("CONFIGURED", 1)
    );
    t.h.internal_json(
        &t.h.peers.telephony,
        Method::POST,
        &format!("/internal/v1/number-resources/{id}/route-readiness"),
        json!({ "family": "VOICE", "routeRef": voice.route_ref, "routeRevision": 1 }),
    )
    .await;
    let ready = |v: &numbering::acquisition::provisioning::AcquisitionStatusView| {
        v.routes
            .iter()
            .find(|r| r.family == RouteFamily::Voice)
            .unwrap()
            .clone()
    };
    assert!(ready(&t.a.acquisition_status(&s, acq).await.unwrap()).ready);
    // The provider binding changes (e.g. provider replacement): the route is stale.
    let binding = t.h.numbers.bindings(id).await.unwrap().pop().unwrap();
    t.h.numbers
        .replace_binding(
            &ctx(),
            binding.id,
            numbering::numbers::bindings::NewBinding {
                provider_id: "synthetic".into(),
                external_ref: "syn-voice-sms-geo".into(),
                capabilities: binding
                    .capabilities
                    .iter()
                    .filter_map(|c| C::parse(c))
                    .collect(),
                change_reason: numbering::numbers::BindingChangeReason::Correction,
            },
        )
        .await
        .unwrap();
    let v = ready(&t.a.acquisition_status(&s, acq).await.unwrap());
    assert_eq!((v.ready, v.reason), (false, Some("BINDING_CHANGED")));
    // Reconfiguration supersedes it (same stable route reference, next revision).
    t.a.configure_routes(&ctx(), &s, acq, &[C::VoiceInbound], "route-unk-03")
        .await
        .unwrap();
    t.drain().await;
    let history = t.h.numbers.route_history(id).await.unwrap();
    assert_eq!(
        history
            .iter()
            .map(|r| (r.route_revision, r.status.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "SUPERSEDED"), (2, "CONFIGURED")]
    );
    assert_eq!(history[0].route_ref, history[1].route_ref);
    let v = ready(&t.a.acquisition_status(&s, acq).await.unwrap());
    assert_eq!((v.ready, v.reason), (false, Some("CONSUMER_NOT_READY")));
    // Configuring routes before provisioning is refused.
    let (_, rid, _) = t.held_geo(&s, "syn-voice-only", &[C::VoiceInbound]).await;
    let pending = t
        .provision(&s, rid, &[C::VoiceInbound], "route-early-1")
        .await
        .unwrap();
    assert_eq!(
        t.a.configure_routes(
            &ctx(),
            &s,
            pending.acquisition.id,
            &[C::VoiceInbound],
            "route-early-2"
        )
        .await
        .unwrap_err()
        .code(),
        "PROVISIONING_REQUIRED"
    );
}

// ---------------------------------------------------------------------------------------
// Database guards, migration rollback, the customer API
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_database_guards_acquisitions_routes_and_readiness() {
    let Some(t) = setup().await else {
        return;
    };
    let s = sandbox();
    let (id, acq) = t.provisioned(&s).await;
    t.a.configure_routes(&ctx(), &s, acq, &[C::VoiceInbound], "guard-route-1")
        .await
        .unwrap();
    for (sql, expected) in [
        (format!("UPDATE number_acquisitions SET binding_id = NULL, revision = revision + 1 WHERE id = '{acq}'"), "final"),
        (format!("UPDATE number_acquisitions SET tenant_id = gen_random_uuid(), revision = revision + 1 WHERE id = '{acq}'"), "immutable"),
        (format!("DELETE FROM number_acquisitions WHERE id = '{acq}'"), "never deleted"),
        (
            format!("UPDATE number_route_configurations SET status = 'CONFIGURED', revision = revision + 1 WHERE number_resource_id = '{id}'"),
            "ck_number_route_configurations_configured",
        ),
        (
            format!("UPDATE number_route_configurations SET route_ref = gen_random_uuid(), revision = revision + 1 WHERE number_resource_id = '{id}'"),
            "immutable",
        ),
        (
            format!("INSERT INTO number_route_readiness (id, route_configuration_id, number_resource_id, family, route_revision, \
               consumer_service, status, correlation_id, acknowledged_at) SELECT gen_random_uuid(), id, number_resource_id, 'VOICE', 1, \
               'cfmeet.calling', 'READY', 'corr_test0001', now() FROM number_route_configurations WHERE number_resource_id = '{id}'"),
            "ck_number_route_readiness_consumer",
        ),
    ] {
        let err = exec(t.h.conn(), &sql).await.unwrap_err().to_string();
        assert!(err.contains(expected), "{sql}: {err}");
    }
    use numbering_migration::{Migrator, MigratorTrait};
    let steps = common::steps_down_to("m20261021_000007_provisioning_activation");
    let err = Migrator::down(t.h.conn(), Some(steps)).await.unwrap_err();
    assert!(err.to_string().contains("Workstream I"), "{err}");
    let Some(clean) = common::TestDb::new().await else {
        return;
    };
    Migrator::down(&clean.conn, Some(steps)).await.unwrap();
    assert_eq!(
        scalar(&clean.conn, "SELECT count(*) FROM information_schema.tables WHERE table_name LIKE 'number_route%' OR table_name = 'number_acquisitions'").await,
        0
    );
    Migrator::up(&clean.conn, None).await.unwrap();
}

#[tokio::test]
async fn the_acquisition_api_is_scoped_isolated_idempotent_and_contract_conformant() {
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
        "numbers.kyc.read",
    ];
    let alice = pa.customer("SANDBOX", &all, &all).await;
    let bob = pa.customer("SANDBOX", &all, &all).await;
    let s = OwnerScope {
        tenant_id: alice.tenant_id,
        application_id: alice.application_id,
        environment_id: alice.environment_id,
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: alice.service_account_id,
    };
    let page = h
        .customer_with(
            &alice.secret,
            Method::GET,
            "/v1/numbers/search?countryCode=US&numberType=GEOGRAPHIC&region=US-CA",
            None,
            &[],
        )
        .await;
    let number = page.body["data"][0]["numberResourceId"]
        .as_str()
        .unwrap()
        .to_string();
    let elig = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/eligibility",
            Some(json!({
                "subjectKind": "ORGANIZATION", "subjectRef": Uuid::new_v4(), "countryCode": "US",
                "numberType": "GEOGRAPHIC", "ownershipClass": "ORGANIZATION",
                "capabilities": ["VOICE_INBOUND"], "numberResourceId": number,
            })),
            &[],
        )
        .await;
    assert_eq!(elig.body["outcome"], "ELIGIBLE", "{:?}", elig.body);
    let res = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/reservations",
            Some(json!({ "numberResourceId": number, "capabilities": ["VOICE_INBOUND"], "eligibilityDecisionId": elig.body["eligibilityDecisionId"] })),
            &[("idempotency-key", "api-res-0001")],
        )
        .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{:?}", res.body);
    ax::drain(&runner).await;
    let rid = res.body["reservationId"].as_str().unwrap();
    let body = json!({ "reservationId": rid, "capabilities": ["VOICE_INBOUND"] });
    let r = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/acquisitions",
            Some(body.clone()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = h
        .customer_with(
            &bob.secret,
            Method::POST,
            "/v1/numbers/acquisitions",
            Some(body.clone()),
            &[("idempotency-key", "api-acq-0001")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let created = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/acquisitions",
            Some(body.clone()),
            &[("idempotency-key", "api-acq-0001")],
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    conforms(&format!("{COMMON}Acquisition"), &created.body);
    let replay = h
        .customer_with(
            &alice.secret,
            Method::POST,
            "/v1/numbers/acquisitions",
            Some(body),
            &[("idempotency-key", "api-acq-0001")],
        )
        .await;
    assert_eq!(
        (
            replay.status,
            replay.headers["idempotent-replayed"].to_str().unwrap()
        ),
        (StatusCode::OK, "true")
    );
    ax::drain(&runner).await;
    let aid = created.body["acquisitionId"].as_str().unwrap();
    let got = h
        .customer_with(
            &alice.secret,
            Method::GET,
            &format!("/v1/numbers/acquisitions/{aid}"),
            None,
            &[],
        )
        .await;
    assert_eq!(got.body["status"], "PROVISIONED");
    conforms(&format!("{COMMON}Acquisition"), &got.body);
    let text = got.body.to_string();
    assert!(!text.contains("syn-") && !text.contains("routeRef") && !text.contains("externalRef"));
    assert_eq!(
        h.customer_with(
            &bob.secret,
            Method::GET,
            &format!("/v1/numbers/acquisitions/{aid}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let routes = h
        .customer_with(
            &alice.secret,
            Method::POST,
            &format!("/v1/numbers/acquisitions/{aid}/routes"),
            Some(json!({ "capabilities": ["VOICE_INBOUND"] })),
            &[("idempotency-key", "api-route-001")],
        )
        .await;
    assert_eq!(routes.status, StatusCode::ACCEPTED, "{:?}", routes.body);
    ax::drain(&runner).await;
    let list = h
        .customer_with(
            &alice.secret,
            Method::GET,
            "/v1/numbers/acquisitions",
            None,
            &[],
        )
        .await;
    conforms(&format!("{COMMON}AcquisitionList"), &list.body);
    assert_eq!(
        list.body["data"][0]["routes"][0]["routeStatus"],
        "CONFIGURED"
    );
    assert_eq!(
        h.customer_with(
            &bob.secret,
            Method::GET,
            "/v1/numbers/acquisitions",
            None,
            &[]
        )
        .await
        .body["data"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let _ = (a.numbers().acquisitions(&s, None, 10).await.unwrap(),);
    let served = h
        .raw(
            axum::http::Request::get("/openapi.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    for path in [
        "/v1/numbers/acquisitions",
        "/v1/numbers/acquisitions/{acquisitionId}",
        "/v1/numbers/acquisitions/{acquisitionId}/routes",
        "/internal/v1/number-resources/{numberResourceId}/routes",
        "/internal/v1/number-resources/{numberResourceId}/route-readiness",
    ] {
        assert!(served.body["paths"].get(path).is_some(), "{path}");
    }
}
