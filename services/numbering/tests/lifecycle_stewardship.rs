//! Part IV Workstreams P–W against the deterministic provider and synthetic inventory:
//!
//! * P — suspension and restoration (local-first, provider route disable, grants refused
//!   and revoked, restoration re-checks, operator-only lifting, a suspension / grant race);
//! * Q — release → quarantine → reassignment (mandatory order, POOL_RETURN and
//!   PROVIDER_RELINQUISH, an unknown provider outcome, late traffic, inventory never
//!   overriding a quarantine, two-worker completion contention, previous-owner isolation);
//! * R — ownership periods and audited, masked ownership verification;
//! * S — idempotent usage / cost evidence;
//! * T — allocation, signal and spend decisions, risk suspension, appeals;
//! * U — portability (feature flag, unsupported, uncertain, port-out keeps routing until
//!   completion);
//! * V — recurring lifecycle drift reconciliation (fail closed);
//! * W — kill switches, and backup / restore resuming release, quarantine and usage
//!   without a duplicate provider action.
mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind,
};
use chrono::Duration;
use common::{acquisition as ax, catalog, kyc as fx, providers as px, scope, Harness, Reply};
use numbering::{
    acquisition::{
        assignments::{AssignmentBasis, AssignmentRequest, ReassignmentRequest},
        provisioning::ProvisionRequest,
        reservations::{customer_actor, ReservationRequest},
        search::{SearchOutcome, SearchRequest},
        Acquisition, AcquisitionSettings,
    },
    domain::RequestContext,
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
    stewardship::{
        controls::Control,
        history::{AccessJustification, AccessPurpose, OwnershipClaim},
        porting::{DeterministicPorting, NewPort, PortDirection, PortOutcome},
        release::{NewRelease, ReleasePolicy},
        risk::{NewSignal, SignalKind},
        suspension::{SuspendRequest, SuspensionSource},
        usage::{NewUsage, UsageKind},
        Stewardship, StewardshipSettings,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

const MOBILE: [C; 3] = [C::VoiceInbound, C::VoiceOutbound, C::SmsInbound];
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
fn org_targets(team: Uuid) -> RoutingTargets {
    RoutingTargets {
        voice: (V::SharedTeam, Some(team)),
        sms: (Sms::SharedTeam, Some(team)),
    }
}
fn delegated(s: &OwnerScope) -> DelegatedPlatformContext {
    let d = DelegatedPlatformContext::new(
        s.tenant_id,
        s.application_id,
        s.environment_id,
        ApplicationEnvironmentKind::Sandbox,
        s.service_account_id,
    );
    d.with_initiating_principal(Uuid::new_v4(), DelegatedPrincipalKind::EnterpriseMember)
}
fn grant_of(r: &Reply) -> String {
    r.body["grant"]["grant"]
        .as_str()
        .or_else(|| r.body["grant"].as_str())
        .unwrap_or_else(|| panic!("no grant: {:?}", r.body))
        .to_string()
}
fn operator() -> Actor {
    Actor::operator("ops.lifecycle")
}
fn rule(e: &DomainError) -> &str {
    e.code()
}

struct L {
    h: Harness,
    p: px::Providers,
    a: Acquisition,
    st: Stewardship,
    runner: numbering::infra::jobs::JobRunner,
    grant_clock: cf_service_auth::ManualClock,
    porting: Arc<DeterministicPorting>,
    kyc: numbering::numbers::kyc::KycService,
}

async fn setup_with(settings: StewardshipSettings) -> Option<L> {
    build(Harness::new().await?, settings).await
}

async fn build(h: Harness, settings: StewardshipSettings) -> Option<L> {
    let (_, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(DeterministicKycVerifier::new()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    let (mut h, grant_clock) = h.with_grants();
    h.state = h.state.clone().with_stewardship_settings(settings.clone());
    h.app = numbering::http::app(h.state.clone());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let dir = Arc::new(ax::Principals::default());
    let mut acq = AcquisitionSettings::default();
    acq.risk = settings.risk.clone();
    let a = ax::of(&h, &p, acq).with_principals(dir.clone());
    let runner =
        ax::runner(&h, &p, &a).with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }));
    let porting = Arc::new(DeterministicPorting::new(&["US"]));
    let st = Stewardship::new(
        h.numbers.clone(),
        Some(p.commands.clone()),
        Arc::new(EligibilityPolicy::default()),
        settings,
    )
    .with_porting(porting.clone());
    Some(L {
        h,
        p,
        a,
        st,
        runner,
        grant_clock,
        porting,
        kyc,
    })
}

async fn setup() -> Option<L> {
    setup_with(StewardshipSettings::default()).await
}

impl L {
    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    fn advance(&self, by: Duration) {
        self.h.clock.advance(by);
        self.grant_clock.advance(by.num_seconds());
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

    async fn e164(&self, number: Uuid) -> String {
        self.h.numbers.resource(number).await.unwrap().e164
    }

    async fn resolve(&self, e164: &str) -> Reply {
        self.h
            .internal_json(
                &self.h.peers.telephony,
                Method::POST,
                RESOLVE,
                json!({ "family": "VOICE", "e164": e164 }),
            )
            .await
    }

    async fn outbound(&self, path: &str, s: &OwnerScope, number: Uuid) -> Reply {
        let peer = if path == CALLER {
            &self.h.peers.calling
        } else {
            &self.h.peers.sms_gateway
        };
        self.h
            .internal_delegated(
                peer,
                path,
                json!({ "numberResourceId": number, "operationRef": op() }),
                delegated(s),
            )
            .await
    }

    async fn verify(&self, peer: &cf_service_auth::ServiceTokenSigner, grant: &str) -> Reply {
        self.h
            .internal_json(
                peer,
                Method::POST,
                VERIFY,
                json!({ "grant": grant, "consume": false }),
            )
            .await
    }

    async fn state(&self, number: Uuid) -> S {
        self.h
            .numbers
            .resource(number)
            .await
            .unwrap()
            .lifecycle_state()
    }

    async fn events(&self, event_type: &str) -> i64 {
        self.h
            .scalar(&format!(
                "SELECT count(*) FROM numbering_outbox_events WHERE event_type = '{event_type}'"
            ))
            .await
    }
}

fn org_suspend(a: &AssignmentRow, reason: &str, source: SuspensionSource) -> SuspendRequest {
    SuspendRequest {
        assignment_id: a.id,
        reason_code: reason.into(),
        source,
        evidence_ref: Some(Uuid::new_v4()),
    }
}

// ---------------------------------------------------------------------------------------
// P — suspension and restoration
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn p_suspension_is_local_first_and_restoration_rechecks_everything() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let e164 = t.e164(number).await;
    let caller = t.outbound(CALLER, &s, number).await;
    assert_eq!(caller.status, StatusCode::OK, "{:?}", caller.body);
    let live = grant_of(&caller);
    let inbound = grant_of(&t.resolve(&e164).await);

    let susp =
        t.st.suspend(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            org_suspend(&a, "CUSTOMER_REQUEST", SuspensionSource::Organization),
        )
        .await
        .unwrap();
    assert_eq!(
        (susp.status.as_str(), susp.provider_state.as_str()),
        ("SUSPENDED", "PENDING")
    );
    // Ownership is retained: the same open assignment, the same owner.
    let held = t.h.numbers.assignment(&s, a.id).await.unwrap();
    assert_eq!(
        (held.status.as_str(), held.owner_ref),
        ("SUSPENDED", a.owner_ref)
    );
    assert_eq!(t.state(number).await, S::Suspended);
    // New grants are refused at once; existing grants no longer verify.
    assert_eq!(
        t.outbound(CALLER, &s, number).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.outbound(SENDER, &s, number).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        t.verify(&t.h.peers.telephony, &live).await.body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(
        t.verify(&t.h.peers.calling, &inbound).await.body["reason"],
        "GRANT_REVOKED"
    );
    assert_eq!(t.resolve(&e164).await.body["outcome"], "UNAVAILABLE");
    assert_eq!(t.events("number.suspended").await, 1);
    // Idempotent.
    let again =
        t.st.suspend(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            org_suspend(&a, "CUSTOMER_REQUEST", SuspensionSource::Organization),
        )
        .await
        .unwrap();
    assert_eq!(again.id, susp.id);
    // Restoration waits for the provider suspension to settle.
    assert_eq!(
        rule(
            &t.st
                .restore(&ctx(), Some(&s), customer_actor(&s), a.id, standing())
                .await
                .unwrap_err()
        ),
        "COMMAND_IN_FLIGHT"
    );
    t.drain().await;
    let settled = t.st.advance_suspension(&ctx(), susp.id).await.unwrap();
    assert_eq!(settled.provider_state, "CONFIRMED");
    // A tenant whose standing lapsed cannot restore; the failed check is recorded.
    let lapsed = Standing {
        active: false,
        ..standing()
    };
    assert_eq!(
        rule(
            &t.st
                .restore(&ctx(), Some(&s), customer_actor(&s), a.id, lapsed)
                .await
                .unwrap_err()
        ),
        "STANDING_NOT_ACTIVE"
    );
    let blocked = t.st.suspension_of(Some(&s), a.id).await.unwrap();
    assert_eq!(
        blocked.restore_blocked_reason.as_deref(),
        Some("STANDING_NOT_ACTIVE")
    );
    assert_eq!(
        blocked.restore_checks.as_ref().unwrap()["standingActive"],
        false
    );
    // Another tenant cannot touch it.
    let other = scope(EnvironmentKind::Sandbox);
    assert!(t
        .st
        .restore(
            &ctx(),
            Some(&other),
            customer_actor(&other),
            a.id,
            standing()
        )
        .await
        .is_err());
    // Restore: RESTORING until the provider confirms, then ACTIVE with number.restored.
    let restoring =
        t.st.restore(&ctx(), Some(&s), customer_actor(&s), a.id, standing())
            .await
            .unwrap();
    assert_eq!(restoring.status, "RESTORING");
    assert_eq!(t.resolve(&e164).await.body["outcome"], "UNAVAILABLE");
    t.drain().await;
    let restored = t.st.advance_suspension(&ctx(), susp.id).await.unwrap();
    assert_eq!(restored.status, "RESTORED");
    assert_eq!(t.state(number).await, S::Active);
    assert_eq!(
        t.h.numbers.assignment(&s, a.id).await.unwrap().status,
        "ACTIVE"
    );
    assert_eq!(t.resolve(&e164).await.body["outcome"], "ROUTED");
    assert_eq!(t.outbound(CALLER, &s, number).await.status, StatusCode::OK);
    assert_eq!(t.events("number.restored").await, 1);
    // The provider saw exactly one SUSPEND and one RESTORE.
    let calls = t.p.sim.calls();
    assert_eq!(calls.iter().filter(|c| c.0 == Op::Suspend).count(), 1);
    assert_eq!(calls.iter().filter(|c| c.0 == Op::Restore).count(), 1);

    // An operator suspension is lifted by an operator only.
    let a = t.h.numbers.assignment(&s, a.id).await.unwrap();
    let ops =
        t.st.suspend(
            &ctx(),
            None,
            operator(),
            org_suspend(&a, "ABUSE_REVIEW", SuspensionSource::Operator),
        )
        .await
        .unwrap();
    t.drain().await;
    t.st.advance_suspension(&ctx(), ops.id).await.unwrap();
    assert_eq!(
        rule(
            &t.st
                .restore(&ctx(), Some(&s), customer_actor(&s), a.id, standing())
                .await
                .unwrap_err()
        ),
        "OPERATOR_RESTORE_REQUIRED"
    );
    t.st.restore(&ctx(), None, operator(), a.id, standing())
        .await
        .unwrap();
    t.drain().await;
    assert_eq!(
        t.st.advance_suspension(&ctx(), ops.id)
            .await
            .unwrap()
            .status,
        "RESTORED"
    );
}

#[tokio::test]
async fn p_a_grant_racing_a_suspension_never_verifies_afterwards() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let t = Arc::new(t);
    let mut grants = Vec::new();
    let suspender = {
        let t = t.clone();
        let s = s;
        tokio::spawn(async move {
            t.st.suspend(
                &ctx(),
                Some(&s),
                customer_actor(&s),
                org_suspend(&a, "CUSTOMER_REQUEST", SuspensionSource::Organization),
            )
            .await
            .unwrap()
        })
    };
    for _ in 0..8 {
        let t = t.clone();
        grants.push(tokio::spawn(
            async move { t.outbound(CALLER, &s, number).await },
        ));
    }
    suspender.await.unwrap();
    for g in grants {
        let r = g.await.unwrap();
        if r.status == StatusCode::OK {
            let v = t.verify(&t.h.peers.telephony, &grant_of(&r)).await;
            assert_eq!(v.body["valid"], false, "{:?}", v.body);
        } else {
            assert_eq!(r.status, StatusCode::NOT_FOUND);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Q — release, quarantine and reassignment
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn q_pool_return_release_quarantine_and_safe_reassignment() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let e164 = t.e164(number).await;
    t.advance(Duration::hours(1));
    let old_grant = grant_of(&t.outbound(CALLER, &s, number).await);
    let (release, replayed) =
        t.st.request_release(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            NewRelease {
                assignment_id: a.id,
                reason_code: "CUSTOMER_RELEASE".into(),
                policy: ReleasePolicy::PoolReturn,
                effective_at: None,
            },
            "release-key-0001",
        )
        .await
        .unwrap();
    assert!(!replayed);
    assert_eq!(release.status, "QUARANTINED");
    let evidence = release.routing_evidence.clone().unwrap();
    assert_eq!(evidence["liveGrants"], 0);
    assert_eq!(evidence["readyRoutes"], 0);
    assert_eq!(evidence["retiredRoutes"].as_array().unwrap().len(), 2);
    assert!(release.grants_stopped_at.unwrap() <= release.routing_removed_at.unwrap());
    // Replay with the same key: the same release.
    let (same, replayed) =
        t.st.request_release(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            NewRelease {
                assignment_id: a.id,
                reason_code: "CUSTOMER_RELEASE".into(),
                policy: ReleasePolicy::PoolReturn,
                effective_at: None,
            },
            "release-key-0001",
        )
        .await
        .unwrap();
    assert!(replayed && same.id == release.id);
    assert_eq!(t.state(number).await, S::Quarantined);
    assert_eq!(
        t.verify(&t.h.peers.telephony, &old_grant).await.body["valid"],
        false
    );
    // A release is never a delete.
    assert!(common::exec(
        t.h.conn(),
        &format!(
            "DELETE FROM number_release_requests WHERE id = '{}'",
            release.id
        )
    )
    .await
    .is_err());
    // Late traffic is refused and recorded, never delivered.
    for _ in 0..3 {
        assert_eq!(t.resolve(&e164).await.body["reason"], "NOT_ACTIVE");
    }
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_late_activity")
            .await,
        3
    );
    // The quarantined number is neither searchable nor reservable for anyone.
    let other = scope(EnvironmentKind::Sandbox);
    let d =
        t.h.numbers
            .evaluate_eligibility(
                &ctx(),
                &other,
                standing(),
                &EligibilityPolicy::default(),
                EligibilityRequest {
                    subject_kind: SubjectKind::Organization,
                    subject_ref: other.tenant_id,
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
    if d.outcome == "ELIGIBLE" {
        assert!(t
            .a
            .create_reservation(
                &ctx(),
                &other,
                ReservationRequest {
                    number_resource_id: number,
                    capabilities: MOBILE.to_vec(),
                    hold_seconds: Some(600),
                    eligibility_decision_id: Some(d.id)
                },
                &key()
            )
            .await
            .is_err());
    }
    // Provider inventory showing the number never overrides the quarantine.
    let _ =
        t.a.search(
            &ctx(),
            &other,
            true,
            &SearchRequest {
                country: "US".into(),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(t.state(number).await, S::Quarantined);
    // Not before the cooldown.
    assert_eq!(t.st.complete_quarantines(&ctx(), 10).await.unwrap(), 0);
    let q = t.st.quarantine_of(number).await.unwrap();
    assert_eq!(
        (q.disposition.as_str(), q.cooldown_days),
        ("POOL_RETURN", 7)
    );
    assert!(matches!(
        t.a.reassign(
            &ctx(),
            &other,
            standing(),
            ReassignmentRequest {
                number_resource_id: number,
                ownership_class: O::Organization,
                owner_principal_id: None,
                targets: org_targets(Uuid::new_v4()),
                capabilities: MOBILE.to_vec(),
                eligibility_decision_id: d.id,
                pool_id: None
            },
            &key()
        )
        .await,
        Err(_)
    ));
    // Two workers race the completion: exactly one completes it.
    t.advance(Duration::days(8));
    let (c1, c2) = (ctx(), ctx());
    let (x, y) = tokio::join!(
        t.st.complete_quarantines(&c1, 10),
        t.st.complete_quarantines(&c2, 10)
    );
    assert_eq!(x.unwrap() + y.unwrap(), 1);
    let q = t.st.quarantine_of(number).await.unwrap();
    assert_eq!(q.status, "COMPLETED");
    assert!(q.provider_confirmed_at.is_some());
    assert_eq!(t.state(number).await, S::Reassignable);
    assert_eq!(
        t.h.numbers.assignment(&s, a.id).await.unwrap().status,
        "CLOSED"
    );
    // Reassignment to a new owner: fresh reservation, assignment and routing revision.
    let before = t.h.numbers.resource(number).await.unwrap().routing_revision;
    let d = t.decide(&other, number).await;
    let team = Uuid::new_v4();
    let b =
        t.a.reassign(
            &ctx(),
            &other,
            standing(),
            ReassignmentRequest {
                number_resource_id: number,
                ownership_class: O::Organization,
                owner_principal_id: None,
                targets: org_targets(team),
                capabilities: MOBILE.to_vec(),
                eligibility_decision_id: d,
                pool_id: None,
            },
            &key(),
        )
        .await
        .unwrap()
        .assignment;
    assert_ne!(b.id, a.id);
    assert!(t.h.numbers.resource(number).await.unwrap().routing_revision > before);
    // Not routable until the NEW routes are configured and acknowledged.
    assert!(t
        .a
        .activate_assignment(&ctx(), &other, standing(), b.id)
        .await
        .is_err());
    t.drain().await;
    t.acknowledge(number).await;
    t.a.activate_assignment(&ctx(), &other, standing(), b.id)
        .await
        .unwrap();
    let r = t.resolve(&e164).await;
    assert_eq!(r.body["outcome"], "ROUTED");
    assert_eq!(r.body["assignmentId"], json!(b.id));
    assert_eq!(r.body["target"]["ref"], json!(team));
    // The previous owner: nothing — no grant, no view, the old grant stays dead.
    assert_eq!(
        t.outbound(CALLER, &s, number).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(t.h.numbers.assignment(&s, b.id).await.is_err());
    assert_eq!(
        t.verify(&t.h.peers.telephony, &old_grant).await.body["valid"],
        false
    );
    // History: two owners, each period bounded.
    let j = AccessJustification {
        purpose: AccessPurpose::AccountRecovery,
        case_ref: "CASE-1001".into(),
    };
    let periods =
        t.st.ownership_periods(&ctx(), &operator(), number, &j)
            .await
            .unwrap();
    assert_eq!(periods.len(), 2);
    assert!(periods[0].ended_at.is_some() && periods[1].ended_at.is_none());
    let during_first = periods[0].started_at.unwrap() + Duration::seconds(1);
    let m =
        t.st.verify_ownership(
            &ctx(),
            &operator(),
            number,
            &OwnershipClaim {
                tenant_id: s.tenant_id,
                owner_ref: s.tenant_id,
                at: during_first,
            },
            &j,
        )
        .await
        .unwrap();
    assert_eq!(m.outcome, "MATCH");
    // Possession of the reassigned number proves nothing about the earlier period.
    let n =
        t.st.verify_ownership(
            &ctx(),
            &operator(),
            number,
            &OwnershipClaim {
                tenant_id: other.tenant_id,
                owner_ref: other.tenant_id,
                at: during_first,
            },
            &j,
        )
        .await
        .unwrap();
    assert_eq!(n.outcome, "NO_MATCH");
    assert!(n.assignment_id.is_none() && n.period_start.is_none());
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_history_access")
            .await,
        3
    );
    // Customers never see other owners; privileged reads need an operator and a case.
    assert!(t
        .st
        .ownership_periods(&ctx(), &customer_actor(&s), number, &j)
        .await
        .is_err());
    assert!(t
        .st
        .ownership_periods(
            &ctx(),
            &operator(),
            number,
            &AccessJustification {
                purpose: AccessPurpose::Audit,
                case_ref: "x".into()
            }
        )
        .await
        .is_err());
    let ev =
        t.st.lifecycle_evidence(&ctx(), &operator(), number, &j)
            .await
            .unwrap();
    assert!(ev["workflow"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["kind"] == "QUARANTINE"));
    assert_eq!(ev["lateActivityEvents"], 3);
}

#[tokio::test]
async fn q_relinquished_release_with_an_unknown_provider_outcome_is_never_reassignable() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    t.p.sim.inject(Op::Release, Fault::TimeoutAfterExecution);
    let (release, _) =
        t.st.request_release(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            NewRelease {
                assignment_id: a.id,
                reason_code: "CUSTOMER_RELEASE".into(),
                policy: ReleasePolicy::ProviderRelinquish,
                effective_at: None,
            },
            "release-key-0002",
        )
        .await
        .unwrap();
    assert_eq!(release.status, "PROVIDER_RELEASE_PENDING");
    t.drain().await;
    let r = t.st.advance_release(&ctx(), release.id).await.unwrap();
    assert_eq!(r.status, "PROVIDER_OUTCOME_UNKNOWN");
    assert_eq!(t.state(number).await, S::UnknownProviderState);
    // Never a second RELEASE: reconciliation reads back.
    t.advance(Duration::minutes(10));
    t.drain().await;
    t.p.commands.reconcile_unknown(&ctx(), 10).await.unwrap();
    let r = t.st.advance_release(&ctx(), release.id).await.unwrap();
    assert_eq!(r.status, "QUARANTINED", "{r:?}");
    assert_eq!(
        t.p.sim
            .calls()
            .iter()
            .filter(|c| c.0 == Op::Release)
            .count(),
        1
    );
    let q = t.st.quarantine_of(number).await.unwrap();
    assert_eq!(q.disposition, "RELINQUISHED");
    // The provider offers the number again: it is not CF Meet's to reassign.
    t.advance(Duration::days(8));
    assert_eq!(t.st.complete_quarantines(&ctx(), 10).await.unwrap(), 1);
    assert_eq!(t.state(number).await, S::Released);
    let other = scope(EnvironmentKind::Sandbox);
    let _ =
        t.a.search(
            &ctx(),
            &other,
            true,
            &SearchRequest {
                country: "US".into(),
                ..Default::default()
            },
        )
        .await;
    assert_ne!(t.state(number).await, S::Reassignable);
    // Its quarantine evidence is permanent: the disposition never changes.
    assert!(common::exec(t.h.conn(), &format!(
        "UPDATE number_quarantines SET disposition = 'POOL_RETURN' WHERE number_resource_id = '{number}'")).await.is_err());
}

#[tokio::test]
async fn q_a_scheduled_release_waits_for_its_effective_date() {
    let Some(t) = setup_with(StewardshipSettings {
        porting_enabled: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let e164 = t.e164(number).await;
    let (r, _) =
        t.st.request_release(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            NewRelease {
                assignment_id: a.id,
                reason_code: "CUSTOMER_RELEASE".into(),
                policy: ReleasePolicy::PoolReturn,
                effective_at: Some(t.h.now() + Duration::hours(2)),
            },
            "release-key-0003",
        )
        .await
        .unwrap();
    assert_eq!(r.status, "REQUESTED");
    assert_eq!(t.resolve(&e164).await.body["outcome"], "ROUTED");
    assert_eq!(t.st.advance_releases(&ctx(), 10).await.unwrap(), 0);
    t.advance(Duration::hours(3));
    t.st.advance_releases(&ctx(), 10).await.unwrap();
    assert_eq!(
        t.st.release_request(Some(&s), r.id).await.unwrap().status,
        "QUARANTINED"
    );
}

// ---------------------------------------------------------------------------------------
// S — usage and billing readiness; T — risk and spend
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn s_usage_is_idempotent_and_t_spend_and_risk_restrict_dcn_traffic_only() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    // Rental accrual: repeated passes add nothing.
    let first = t.st.accrue_usage(&ctx(), 100).await.unwrap();
    assert!(first >= 1);
    assert_eq!(t.st.accrue_usage(&ctx(), 100).await.unwrap(), 0);
    // A carrier's report counts once; the same key with different facts is refused.
    let usage = |qty: i64, cost: i64| NewUsage {
        source_key: "sms-msg-0001".into(),
        number_resource_id: number,
        assignment_id: Some(a.id),
        kind: UsageKind::SmsInboundSegments,
        quantity: qty,
        provider_cost_minor: Some(cost),
        currency: Some("USD".into()),
        period_start: t.h.now(),
        period_end: t.h.now(),
    };
    assert!(
        t.st.report_usage(&ctx(), "cfmeet.sms-gateway", usage(2, 300))
            .await
            .unwrap()
            .1
    );
    assert!(
        !t.st
            .report_usage(&ctx(), "cfmeet.sms-gateway", usage(2, 300))
            .await
            .unwrap()
            .1
    );
    assert_eq!(
        rule(
            &t.st
                .report_usage(&ctx(), "cfmeet.sms-gateway", usage(3, 300))
                .await
                .unwrap_err()
        ),
        "USAGE_SOURCE_CONFLICT"
    );
    // Only the carrier of the family reports it.
    assert_eq!(
        rule(
            &t.st
                .report_usage(&ctx(), "cfmeet.telephony", usage(2, 300))
                .await
                .unwrap_err()
        ),
        "USAGE_KIND_NOT_PERMITTED"
    );
    let summary =
        t.st.usage_summary(
            &s,
            t.h.now() - Duration::days(40),
            t.h.now() + Duration::days(40),
        )
        .await
        .unwrap();
    assert!(summary
        .iter()
        .any(|u| u.usage_kind == "SMS_INBOUND_SEGMENTS" && u.quantity == 2));
    let other = scope(EnvironmentKind::Sandbox);
    assert!(t
        .st
        .usage_summary(
            &other,
            t.h.now() - Duration::days(40),
            t.h.now() + Duration::days(40)
        )
        .await
        .unwrap()
        .is_empty());
    // Finalized records are immutable.
    t.advance(Duration::hours(1));
    assert!(t.st.finalize_usage(t.h.now()).await.unwrap() >= 1);
    assert!(common::exec(t.h.conn(), "UPDATE number_usage_records SET quantity = 99 WHERE source_key = 'cfmeet.sms-gateway:sms-msg-0001'").await.is_err());
    // Spend: past the threshold no new outbound grant.
    let big = NewUsage {
        source_key: "call-leg-0001".into(),
        kind: UsageKind::VoiceOutboundSeconds,
        quantity: 3600,
        provider_cost_minor: Some(60_000),
        ..usage(1, 0)
    };
    t.st.report_usage(&ctx(), "cfmeet.telephony", big)
        .await
        .unwrap();
    let r = t.outbound(CALLER, &s, number).await;
    assert_eq!(
        r.body["error"]["reason"], "SPEND_LIMIT_REACHED",
        "{:?}",
        r.body
    );

    // T: a pumping signal restricts and suspends the number (source RISK).
    let out =
        t.st.report_signal(
            &ctx(),
            "cfmeet.sms-gateway",
            NewSignal {
                number_resource_id: number,
                kind: SignalKind::SmsPumping,
                magnitude: 95,
                source_key: "pump-0001".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(out.outcome, "RESTRICT");
    assert!(out.suspension_id.is_some());
    // A retry of the same signal adds nothing.
    let again =
        t.st.report_signal(
            &ctx(),
            "cfmeet.sms-gateway",
            NewSignal {
                number_resource_id: number,
                kind: SignalKind::SmsPumping,
                magnitude: 95,
                source_key: "pump-0001".into(),
            },
        )
        .await
        .unwrap();
    assert!(!again.recorded);
    // Telephony cannot report SMS pumping.
    assert!(t
        .st
        .report_signal(
            &ctx(),
            "cfmeet.telephony",
            NewSignal {
                number_resource_id: number,
                kind: SignalKind::SmsPumping,
                magnitude: 95,
                source_key: "pump-0002".into()
            }
        )
        .await
        .is_err());
    let susp = t.st.suspension_of(Some(&s), a.id).await.unwrap();
    assert_eq!(susp.source, "RISK");
    t.drain().await;
    t.st.advance_suspension(&ctx(), susp.id).await.unwrap();
    // The customer cannot lift it; the operator cannot while the restriction is in force.
    assert_eq!(
        rule(
            &t.st
                .restore(&ctx(), Some(&s), customer_actor(&s), a.id, standing())
                .await
                .unwrap_err()
        ),
        "OPERATOR_RESTORE_REQUIRED"
    );
    assert_eq!(
        rule(
            &t.st
                .restore(&ctx(), None, operator(), a.id, standing())
                .await
                .unwrap_err()
        ),
        "RISK_RESTRICTED"
    );
    // Appeal: appended, the original decision untouched; upheld → restorable.
    let original =
        t.st.decision(Some(&s), out.decision_id.unwrap())
            .await
            .unwrap();
    let appeal =
        t.st.file_appeal(&ctx(), &s, customer_actor(&s), original.id, None)
            .await
            .unwrap();
    assert_eq!(appeal.outcome, "PENDING");
    assert!(t
        .st
        .file_appeal(&ctx(), &s, customer_actor(&s), original.id, None)
        .await
        .is_err());
    assert!(t
        .st
        .decide_appeal(&ctx(), &customer_actor(&s), appeal.id, true)
        .await
        .is_err());
    t.st.decide_appeal(&ctx(), &operator(), appeal.id, true)
        .await
        .unwrap();
    assert_eq!(
        t.st.decision(Some(&s), original.id).await.unwrap(),
        original
    );
    assert!(common::exec(
        t.h.conn(),
        "UPDATE number_risk_decisions SET outcome = 'ALLOW'"
    )
    .await
    .is_err());
    // Spend is still above the threshold, but restoration itself is permitted now.
    t.st.restore(&ctx(), None, operator(), a.id, standing())
        .await
        .unwrap();
    t.drain().await;
    assert_eq!(
        t.st.advance_suspension(&ctx(), susp.id)
            .await
            .unwrap()
            .status,
        "RESTORED"
    );
}

#[tokio::test]
async fn t_allocation_risk_denies_restricted_countries_and_velocity_with_reasons() {
    let mut settings = StewardshipSettings::default();
    settings.risk.restricted_countries.insert("US".into());
    let Some(t) = setup_with(settings).await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let number = t.id_of(&s, "syn-voice-sms-geo").await;
    let d = t.decide(&s, number).await;
    let e =
        t.a.create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: number,
                capabilities: MOBILE.to_vec(),
                hold_seconds: Some(600),
                eligibility_decision_id: Some(d),
            },
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(rule(&e), "RISK_DENIED");
    let reasons = common::strings(t.h.conn(), "SELECT array_to_string(reasons, ',') FROM number_risk_decisions WHERE decision_kind = 'ALLOCATION'").await;
    assert_eq!(reasons, vec!["COUNTRY_RESTRICTED".to_string()]);

    let mut settings = StewardshipSettings::default();
    settings.risk.daily_allocations = 1;
    let Some(t) = setup_with(settings).await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let (_, _) = t.active(&s).await;
    let n2 = t.id_of(&s, "syn-voice-only").await;
    let d2 =
        t.h.numbers
            .evaluate_eligibility(
                &ctx(),
                &s,
                standing(),
                &EligibilityPolicy::default(),
                EligibilityRequest {
                    subject_kind: SubjectKind::Organization,
                    subject_ref: s.tenant_id,
                    country: "US".into(),
                    number_type: NumberType::Geographic,
                    ownership_class: O::Organization,
                    capabilities: vec![C::VoiceInbound],
                    number_resource_id: Some(n2),
                    provider_id: None,
                },
            )
            .await
            .unwrap();
    let e =
        t.a.create_reservation(
            &ctx(),
            &s,
            ReservationRequest {
                number_resource_id: n2,
                capabilities: vec![C::VoiceInbound],
                hold_seconds: Some(600),
                eligibility_decision_id: Some(d2.id),
            },
            &key(),
        )
        .await
        .unwrap_err();
    assert_eq!(rule(&e), "RISK_DENIED");
}

// ---------------------------------------------------------------------------------------
// U — portability
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn u_porting_is_flagged_unsupported_or_uncertain_and_port_out_keeps_routing_until_completion()
{
    // Disabled by default.
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let e =
        t.st.request_port(
            &ctx(),
            &s,
            customer_actor(&s),
            NewPort {
                direction: PortDirection::In,
                number: Some("+12025550199".into()),
                country: Some("US".into()),
                assignment_id: None,
                ownership_class: O::Organization,
                kyc_case_id: None,
                evidence_refs: vec![],
            },
            "port-key-0001",
        )
        .await
        .unwrap_err();
    assert_eq!(rule(&e), "PORTING_DISABLED");

    let Some(t) = setup_with(StewardshipSettings {
        porting_enabled: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let e164 = t.e164(number).await;
    // An unsupported country is rejected.
    let (gb, _) =
        t.st.request_port(
            &ctx(),
            &s,
            customer_actor(&s),
            NewPort {
                direction: PortDirection::In,
                number: Some("+442079460000".into()),
                country: Some("GB".into()),
                assignment_id: None,
                ownership_class: O::Organization,
                kyc_case_id: None,
                evidence_refs: vec![],
            },
            "port-key-0002",
        )
        .await
        .unwrap();
    assert_eq!(
        (gb.status.as_str(), gb.status_reason.as_deref()),
        ("REJECTED", Some("PROVIDER_UNSUPPORTED"))
    );
    // Port-out without ownership evidence: documents required.
    let (out, _) =
        t.st.request_port(
            &ctx(),
            &s,
            customer_actor(&s),
            NewPort {
                direction: PortDirection::Out,
                number: None,
                country: None,
                assignment_id: Some(a.id),
                ownership_class: O::Organization,
                kyc_case_id: None,
                evidence_refs: vec![],
            },
            "port-key-0003",
        )
        .await
        .unwrap();
    assert_eq!(out.status, "DOCUMENTS_REQUIRED");
    // While a port-out is open the owner cannot also release.
    assert!(t
        .st
        .request_release(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            NewRelease {
                assignment_id: a.id,
                reason_code: "CUSTOMER_RELEASE".into(),
                policy: ReleasePolicy::PoolReturn,
                effective_at: None
            },
            "release-key-0004"
        )
        .await
        .is_err());
    // Protected ownership evidence of the scope (a sealed KYC evidence reference) →
    // eligible → an uncertain submission.
    let case = t
        .kyc
        .create_case(
            &ctx(),
            &s,
            NewCase {
                subject_kind: SubjectKind::Organization,
                subject_ref: s.tenant_id,
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
    let evidence = t
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            EvidenceKind::BusinessRegistrationCertificate,
            "application/pdf",
            EvidenceBytes::new(fx::pdf("ownership")),
            None,
        )
        .await
        .unwrap()
        .value;
    // Another scope's evidence never counts.
    let stranger = scope(EnvironmentKind::Sandbox);
    assert!(t
        .st
        .add_port_evidence(
            &ctx(),
            &stranger,
            customer_actor(&stranger),
            out.id,
            vec![evidence.id],
            None
        )
        .await
        .is_err());
    let out =
        t.st.add_port_evidence(
            &ctx(),
            &s,
            customer_actor(&s),
            out.id,
            vec![evidence.id],
            None,
        )
        .await
        .unwrap();
    assert_eq!(out.status, "ELIGIBILITY_CHECK", "{out:?}");
    t.porting.script_submit(PortOutcome::Unknown);
    let sub =
        t.st.submit_port(&ctx(), &s, customer_actor(&s), out.id)
            .await
            .unwrap();
    assert_eq!(sub.status, "UNKNOWN_PROVIDER_STATE");
    // Routing is untouched before authoritative completion.
    assert_eq!(t.resolve(&e164).await.body["outcome"], "ROUTED");
    t.porting.script_status(PortOutcome::InProgress);
    t.st.advance_ports(&ctx(), 10).await.unwrap();
    assert_eq!(
        t.st.port_request(Some(&s), out.id).await.unwrap().status,
        "IN_PROGRESS"
    );
    assert_eq!(t.resolve(&e164).await.body["outcome"], "ROUTED");
    t.porting.script_status(PortOutcome::Completed);
    t.st.advance_ports(&ctx(), 10).await.unwrap();
    let done = t.st.port_request(Some(&s), out.id).await.unwrap();
    assert_eq!(done.status, "COMPLETED");
    assert_eq!(
        *t.porting.submissions.lock().unwrap(),
        1,
        "never resubmitted"
    );
    assert_eq!(t.state(number).await, S::Quarantined);
    assert_eq!(
        t.st.quarantine_of(number).await.unwrap().disposition,
        "RELINQUISHED"
    );
    assert_eq!(t.resolve(&e164).await.body["outcome"], "UNAVAILABLE");
    let events = common::strings(t.h.conn(), &format!(
        "SELECT to_status FROM number_port_events WHERE port_request_id = '{}' ORDER BY sequence", out.id)).await;
    assert_eq!(events.first().map(String::as_str), Some("REQUESTED"));
    assert_eq!(events.last().map(String::as_str), Some("COMPLETED"));
}

// ---------------------------------------------------------------------------------------
// V — reconciliation; W — kill switches
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn v_provider_drift_fails_closed_and_w_kill_switches_stop_dcn_traffic() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let e164 = t.e164(number).await;
    // W: the emergency inbound switch; app-to-app is not Numbering's at all.
    t.st.set_control(
        &operator(),
        Control::DcnInboundRouting,
        true,
        "CARRIER_INCIDENT",
    )
    .await
    .unwrap();
    assert_eq!(t.resolve(&e164).await.body["reason"], "ROUTING_DISABLED");
    assert!(t
        .st
        .set_control(
            &customer_actor(&s),
            Control::DcnInboundRouting,
            false,
            "X_X"
        )
        .await
        .is_err());
    t.st.set_control(
        &operator(),
        Control::DcnInboundRouting,
        false,
        "CARRIER_RECOVERED",
    )
    .await
    .unwrap();
    assert_eq!(t.resolve(&e164).await.body["outcome"], "ROUTED");
    let g = grant_of(&t.outbound(CALLER, &s, number).await);
    t.st.set_control(
        &operator(),
        Control::DcnCallerIdentity,
        true,
        "SPOOFING_INCIDENT",
    )
    .await
    .unwrap();
    assert_eq!(
        t.outbound(CALLER, &s, number).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        t.verify(&t.h.peers.telephony, &g).await.body["reason"],
        "ROUTING_DISABLED"
    );
    t.st.set_control(&operator(), Control::DcnCallerIdentity, false, "RESOLVED")
        .await
        .unwrap();
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_control_events")
            .await,
        4
    );

    // V: a clean sweep changes nothing.
    let report = t.st.reconcile_lifecycle(&ctx(), 100).await.unwrap();
    assert_eq!((report.drift, report.fail_closed), (0, 0), "{report:?}");
    // The provider takes the number away: incident and a fail-closed PROVIDER suspension.
    let ext = common::strings(t.h.conn(), &format!(
        "SELECT external_ref FROM number_provider_bindings WHERE number_resource_id = '{number}' AND status = 'ACTIVE'")).await.pop().unwrap();
    t.p.sim.revoke(&ext);
    let _ = t.p.sim.take_callbacks();
    let report = t.st.reconcile_lifecycle(&ctx(), 100).await.unwrap();
    assert_eq!((report.drift, report.fail_closed), (1, 1), "{report:?}");
    assert_eq!(t.state(number).await, S::Suspended);
    assert_eq!(
        t.st.suspension_of(Some(&s), a.id).await.unwrap().source,
        "PROVIDER"
    );
    assert_eq!(t.h.scalar("SELECT count(*) FROM numbering_incidents WHERE category = 'PROVIDER_RELEASED_UNEXPECTEDLY' AND status = 'OPEN'").await, 1);
    // A second sweep raises no second incident and repeats nothing.
    t.st.reconcile_lifecycle(&ctx(), 100).await.unwrap();
    assert_eq!(t.h.scalar("SELECT count(*) FROM numbering_incidents WHERE category = 'PROVIDER_RELEASED_UNEXPECTEDLY' AND status = 'OPEN'").await, 1);
    // The customer can never restore a provider suspension.
    assert!(t
        .st
        .restore(&ctx(), Some(&s), customer_actor(&s), a.id, standing())
        .await
        .is_err());
}

#[tokio::test]
async fn database_guards_hold_the_quarantine_and_service_class_invariants() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    t.st.request_release(
        &ctx(),
        Some(&s),
        customer_actor(&s),
        NewRelease {
            assignment_id: a.id,
            reason_code: "CUSTOMER_RELEASE".into(),
            policy: ReleasePolicy::PoolReturn,
            effective_at: None,
        },
        "release-key-0005",
    )
    .await
    .unwrap();
    // Forcing REASSIGNABLE during an active quarantine is refused by the database.
    let forced = common::in_txn(t.h.conn(), &[
        "SELECT set_config('numbering.lifecycle', 'transition', true)",
        &format!("UPDATE number_resources SET state = 'REASSIGNABLE', revision = revision + 1, quarantine_until = now() - interval '1 day' WHERE id = '{number}'"),
    ]).await;
    assert!(forced.is_err());
    // Evidence records are append-only / never deleted.
    assert!(common::exec(t.h.conn(), "DELETE FROM number_quarantines")
        .await
        .is_err());
    let q = t.st.quarantine_of(number).await.unwrap();
    assert!(common::exec(
        t.h.conn(),
        &format!(
            "UPDATE number_quarantines SET disposition = 'RELINQUISHED' WHERE id = '{}'",
            q.id
        )
    )
    .await
    .is_err());
    // A relay-class number is never reserved or assigned as a DCN.
    let relay =
        t.st.import_relay_number(
            &ctx(),
            "cfmeet.privacy-relay",
            numbering::stewardship::relay::RelayImport {
                relay_inventory_ref: Uuid::new_v4(),
                e164: "+12025550142".into(),
                country: "US".into(),
                region: None,
                capabilities: vec![C::VoiceInbound, C::VoiceOutbound],
                binding_ref: "relay-binding-guard".into(),
                synthetic: true,
            },
        )
        .await
        .unwrap()
        .0;
    let refused =
        t.h.numbers
            .reserve(
                &ctx(),
                &s,
                customer_actor(&s),
                relay.number_resource_id,
                Duration::minutes(10),
                None,
            )
            .await;
    assert!(refused.is_err());
    let v: Value = json!(relay.number_resource_id);
    assert!(v.is_string());
}

// ---------------------------------------------------------------------------------------
// The HTTP surface: customer / operator authorization through a real Platform Access,
// internal permissions, the served contract.
// ---------------------------------------------------------------------------------------

const COMMON: &str = "numbering/common/v1.schema.json#/$defs/";

#[tokio::test]
async fn http_customer_and_operator_routes_are_scoped_audited_and_conform() {
    use common::contracts::conforms;
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let Some(t) = build(h, StewardshipSettings::default()).await else {
        return;
    };
    let pa = t.h.pa.clone().unwrap();
    let all = [
        "numbers.search",
        "numbers.read",
        "numbers.manage",
        "numbers.assign",
        "numbers.kyc.read",
    ];
    let alice = pa.customer("SANDBOX", &all, &all).await;
    let bob = pa.customer("SANDBOX", &all, &all).await;
    let reader = pa.customer("SANDBOX", &all, &["numbers.read"]).await;
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
    let (number, a) = t.active(&s).await;
    let call = |token: String,
                method: Method,
                path: String,
                body: Option<Value>,
                headers: Vec<(&'static str, String)>| {
        let h = &t.h;
        async move {
            let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            h.customer_with(&token, method, &path, body, &hs).await
        }
    };
    let suspend = format!("/v1/numbers/assignments/{}/suspend", a.id);
    // Another tenant: indistinguishable from missing; a reader: insufficient scope.
    let r = call(
        bob.secret.clone(),
        Method::POST,
        suspend.clone(),
        Some(json!({ "reasonCode": "CUSTOMER_REQUEST" })),
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        reader.secret.clone(),
        Method::POST,
        suspend.clone(),
        Some(json!({ "reasonCode": "CUSTOMER_REQUEST" })),
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(
        alice.secret.clone(),
        Method::POST,
        suspend.clone(),
        Some(json!({ "reasonCode": "CUSTOMER_REQUEST" })),
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}Suspension"), &r.body);
    assert_eq!(r.body["source"], "ORGANIZATION");
    let r = call(
        alice.secret.clone(),
        Method::GET,
        format!("/v1/numbers/assignments/{}/suspension", a.id),
        None,
        vec![],
    )
    .await;
    conforms(&format!("{COMMON}Suspension"), &r.body);
    t.drain().await;
    t.st.advance_suspensions(&ctx(), 10).await.unwrap();
    let r = call(
        alice.secret.clone(),
        Method::POST,
        format!("/v1/numbers/assignments/{}/restore", a.id),
        None,
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["status"], "RESTORING");
    t.drain().await;
    t.st.advance_suspensions(&ctx(), 10).await.unwrap();
    // Release needs an Idempotency-Key; replays answer 200.
    let release = format!("/v1/numbers/assignments/{}/release", a.id);
    let body = json!({ "reasonCode": "CUSTOMER_RELEASE", "policy": "POOL_RETURN" });
    let r = call(
        alice.secret.clone(),
        Method::POST,
        release.clone(),
        Some(body.clone()),
        vec![],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let k = vec![("idempotency-key", "release-http-0001".to_string())];
    let r = call(
        alice.secret.clone(),
        Method::POST,
        release.clone(),
        Some(body.clone()),
        k.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    conforms(&format!("{COMMON}Release"), &r.body);
    assert_eq!(r.body["status"], "QUARANTINED");
    let again = call(
        alice.secret.clone(),
        Method::POST,
        release.clone(),
        Some(body),
        k,
    )
    .await;
    assert_eq!(
        (again.status, again.body["releaseId"].clone()),
        (StatusCode::OK, r.body["releaseId"].clone())
    );
    let rid = r.body["releaseId"].as_str().unwrap().to_string();
    assert_eq!(
        call(
            bob.secret.clone(),
            Method::GET,
            format!("/v1/numbers/releases/{rid}"),
            None,
            vec![]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let q = call(
        alice.secret.clone(),
        Method::GET,
        format!("/v1/numbers/assignments/{}/quarantine", a.id),
        None,
        vec![],
    )
    .await;
    conforms(&format!("{COMMON}Quarantine"), &q.body);
    // Usage: quantities only, never provider costs; another scope sees nothing.
    t.st.accrue_usage(&ctx(), 10).await.unwrap();
    let window = format!(
        "/v1/numbers/usage?from={}&to={}",
        (t.h.now() - Duration::days(40)).format("%Y-%m-%dT%H:%M:%SZ"),
        (t.h.now() + Duration::days(40)).format("%Y-%m-%dT%H:%M:%SZ")
    );
    let u = call(
        alice.secret.clone(),
        Method::GET,
        window.clone(),
        None,
        vec![],
    )
    .await;
    assert_eq!(u.status, StatusCode::OK, "{:?}", u.body);
    conforms(&format!("{COMMON}UsageSummaryList"), &u.body);
    assert!(!u.body.to_string().contains("Cost"));
    assert_eq!(
        call(bob.secret.clone(), Method::GET, window, None, vec![])
            .await
            .body["data"],
        json!([])
    );
    // Porting is off by default.
    let p = call(alice.secret.clone(), Method::POST, "/v1/numbers/ports".into(),
        Some(json!({ "direction": "IN", "e164": "+12025550188", "country": "US", "ownershipClass": "ORGANIZATION" })),
        vec![("idempotency-key", "port-http-0001".to_string())]).await;
    assert_eq!(p.body["error"]["reason"], "PORTING_DISABLED");
    // Operators: numbers.admin on a first-party application AND an operator header.
    let controls = "/v1/admin/controls/DCN_INBOUND_ROUTING".to_string();
    let change = json!({ "engaged": true, "reasonCode": "CARRIER_INCIDENT" });
    assert_eq!(
        call(
            alice.secret.clone(),
            Method::POST,
            controls.clone(),
            Some(change.clone()),
            vec![]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let r = call(
        admin.secret.clone(),
        Method::POST,
        controls.clone(),
        Some(change),
        vec![],
    )
    .await;
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
    let op_header = vec![("x-cf-operator", "ops.alice".to_string())];
    let r = call(
        admin.secret.clone(),
        Method::POST,
        controls.clone(),
        Some(json!({ "engaged": true, "reasonCode": "CARRIER_INCIDENT" })),
        op_header.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}OperationalControl"), &r.body);
    let list = call(
        admin.secret.clone(),
        Method::GET,
        "/v1/admin/controls".into(),
        None,
        op_header.clone(),
    )
    .await;
    conforms(&format!("{COMMON}OperationalControlList"), &list.body);
    call(
        admin.secret.clone(),
        Method::POST,
        controls,
        Some(json!({ "engaged": false, "reasonCode": "RESOLVED" })),
        op_header.clone(),
    )
    .await;
    // Audited, masked history: the justification is mandatory.
    let periods = format!("/v1/admin/number-resources/{number}/ownership-periods");
    let r = call(
        admin.secret.clone(),
        Method::POST,
        periods.clone(),
        Some(json!({})),
        op_header.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        admin.secret.clone(),
        Method::POST,
        periods,
        Some(json!({ "purpose": "ACCOUNT_RECOVERY", "caseRef": "CASE-2001" })),
        op_header.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}OwnershipPeriodList"), &r.body);
    assert_eq!(r.body["data"][0]["tenantId"], json!(alice.tenant_id));
    assert!(!r.body.to_string().contains("ownerRef"));
    let v = call(admin.secret.clone(), Method::POST, format!("/v1/admin/number-resources/{number}/ownership-verifications"),
        Some(json!({ "tenantId": bob.tenant_id, "ownerRef": bob.tenant_id, "at": t.h.now().to_rfc3339(), "purpose": "LEGAL_REQUEST", "caseRef": "CASE-2002" })),
        op_header.clone()).await;
    conforms(&format!("{COMMON}OwnershipVerification"), &v.body);
    assert_eq!(v.body["outcome"], "NO_MATCH");
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_history_access WHERE operator_ref = 'ops.alice'")
            .await,
        2
    );
    let rep = call(
        admin.secret.clone(),
        Method::POST,
        "/v1/admin/reconciliation/lifecycle".into(),
        None,
        op_header,
    )
    .await;
    conforms(&format!("{COMMON}LifecycleReconciliationReport"), &rep.body);
}

#[tokio::test]
async fn http_internal_usage_signals_and_relay_leases_enforce_their_permissions() {
    use common::contracts::conforms;
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let usage = |kind: &str, key: &str| {
        json!({
            "sourceKey": key, "numberResourceId": number, "assignmentId": a.id, "usageKind": kind,
            "quantity": 61, "providerCostMinor": 12, "currency": "USD",
            "periodStart": t.h.now().to_rfc3339(), "periodEnd": t.h.now().to_rfc3339(),
        })
    };
    let r =
        t.h.internal_json(
            &t.h.peers.telephony,
            Method::POST,
            "/internal/v1/number-usage",
            usage("VOICE_OUTBOUND_SECONDS", "leg-1"),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    conforms(&format!("{COMMON}UsageReportResult"), &r.body);
    let r =
        t.h.internal_json(
            &t.h.peers.telephony,
            Method::POST,
            "/internal/v1/number-usage",
            usage("VOICE_OUTBOUND_SECONDS", "leg-1"),
        )
        .await;
    assert_eq!(
        (r.status, r.body["recorded"].clone()),
        (StatusCode::OK, json!(false))
    );
    let r =
        t.h.internal_json(
            &t.h.peers.sms_gateway,
            Method::POST,
            "/internal/v1/number-usage",
            usage("VOICE_OUTBOUND_SECONDS", "leg-2"),
        )
        .await;
    assert_eq!(r.body["error"]["reason"], "USAGE_KIND_NOT_PERMITTED");
    let r =
        t.h.internal_json(
            &t.h.peers.calling,
            Method::POST,
            "/internal/v1/number-usage",
            usage("VOICE_OUTBOUND_SECONDS", "leg-3"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = t.h.internal_json(&t.h.peers.calling, Method::POST, "/internal/v1/number-risk-signals",
        json!({ "numberResourceId": number, "signalKind": "SPAM_CALLS", "magnitude": 30, "sourceKey": "spam-1" })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}RiskSignalResult"), &r.body);
    // Relay inventory: Privacy Relay only.
    let relay_ref = Uuid::new_v4();
    let import = json!({ "relayInventoryRef": relay_ref, "e164": "+12025550177", "country": "US",
        "capabilities": ["VOICE_INBOUND", "VOICE_OUTBOUND"], "bindingRef": "relay-binding-http-1" });
    let r =
        t.h.internal_json(
            &t.h.peers.telephony,
            Method::POST,
            "/internal/v1/relay-inventory/imports",
            import.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r =
        t.h.internal_json(
            &t.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-inventory/imports",
            import.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    conforms(&format!("{COMMON}RelayImportResult"), &r.body);
    let r =
        t.h.internal_json(
            &t.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-inventory/imports",
            import,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let lease = |holder: Uuid, key: &str| {
        json!({ "relayInventoryRef": relay_ref, "holderRef": holder, "generation": 1,
        "expiresAt": (t.h.now() + Duration::hours(1)).to_rfc3339(), "idempotencyKey": key })
    };
    let first =
        t.h.internal_json(
            &t.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-leases",
            lease(Uuid::new_v4(), "lease-key-0001"),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    conforms(&format!("{COMMON}RelayLeaseResult"), &first.body);
    let second =
        t.h.internal_json(
            &t.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-leases",
            lease(Uuid::new_v4(), "lease-key-0002"),
        )
        .await;
    assert_eq!(
        second.body["error"]["reason"], "RELAY_NUMBER_LEASED",
        "{:?}",
        second.body
    );
    let res =
        t.h.internal(
            &t.h.peers.relay,
            Method::GET,
            &format!("/internal/v1/relay-numbers/{relay_ref}/resolution"),
        )
        .await;
    conforms(&format!("{COMMON}RelayResolution"), &res.body);
    assert_eq!(res.body["lease"]["leaseId"], first.body["lease"]["leaseId"]);
    assert!(!res.body.to_string().contains("+1202"));
    let lid = first.body["lease"]["leaseId"].as_str().unwrap().to_string();
    let rel =
        t.h.internal_json(
            &t.h.peers.relay,
            Method::POST,
            &format!("/internal/v1/relay-leases/{lid}/release"),
            json!({ "reasonCode": "SESSION_ENDED" }),
        )
        .await;
    assert_eq!(rel.body["lease"]["status"], "RELEASED");
    // The kill switch stops new leases.
    t.st.set_control(&operator(), Control::RelayLeases, true, "CARRIER_INCIDENT")
        .await
        .unwrap();
    let r =
        t.h.internal_json(
            &t.h.peers.relay,
            Method::POST,
            "/internal/v1/relay-leases",
            lease(Uuid::new_v4(), "lease-key-0003"),
        )
        .await;
    assert_eq!(r.body["error"]["reason"], "OPERATION_DISABLED");
}

// ---------------------------------------------------------------------------------------
// W — backup / restore: a restored database resumes stewardship without duplicate paid
// actions. Requires pg_dump / pg_restore / psql (SKIPPED otherwise, unless
// DISASTER_RECOVERY_CERT_REQUIRED=1).
// ---------------------------------------------------------------------------------------

fn dr_tools() -> bool {
    let ok = ["pg_dump", "pg_restore", "psql", "sha256sum"]
        .iter()
        .all(|t| {
            std::process::Command::new("sh")
                .args(["-c", &format!("command -v {t}")])
                .output()
                .is_ok_and(|o| o.status.success())
        });
    if !ok {
        assert!(
            std::env::var("DISASTER_RECOVERY_CERT_REQUIRED").as_deref() != Ok("1"),
            "disaster-recovery certification REQUIRED but pg_dump / pg_restore / psql are missing"
        );
        eprintln!("SKIPPED: disaster-recovery certification (pg_dump not found)");
    }
    ok
}

fn dr_script(name: &str, args: &[&str]) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/deployment/backup")
        .join(name);
    let out = std::process::Command::new("bash")
        .arg(path)
        .args(args)
        .output()
        .expect("run backup script");
    assert!(
        out.status.success(),
        "{name} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn w_a_restored_database_resumes_release_and_quarantine_without_duplicate_provider_actions() {
    if !dr_tools() {
        return;
    }
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    // A carrier usage report, and a relinquishing release whose provider command is
    // queued but not yet dispatched when the backup is taken.
    let usage = NewUsage {
        source_key: "sms-msg-dr-0001".into(),
        number_resource_id: number,
        assignment_id: Some(a.id),
        kind: UsageKind::SmsInboundSegments,
        quantity: 2,
        provider_cost_minor: Some(300),
        currency: Some("USD".into()),
        period_start: t.h.now(),
        period_end: t.h.now(),
    };
    assert!(
        t.st.report_usage(&ctx(), "cfmeet.sms-gateway", usage.clone())
            .await
            .unwrap()
            .1
    );
    let (release, _) =
        t.st.request_release(
            &ctx(),
            Some(&s),
            customer_actor(&s),
            NewRelease {
                assignment_id: a.id,
                reason_code: "CUSTOMER_RELEASE".into(),
                policy: ReleasePolicy::ProviderRelinquish,
                effective_at: None,
            },
            "release-key-dr-01",
        )
        .await
        .unwrap();
    assert_eq!(release.status, "PROVIDER_RELEASE_PENDING");
    let releases = || {
        t.p.sim
            .calls()
            .iter()
            .filter(|c| c.0 == Op::Release)
            .count()
    };
    assert_eq!(releases(), 0);

    // Backup with the release tooling, restore into an empty database.
    let dir = std::env::temp_dir().join(format!("nb-dr-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let dump = dir.join("numbering.dump");
    dr_script(
        "backup.sh",
        &[
            "--service",
            "numbering",
            "--database-url",
            &t.h.db.url,
            "--out",
            dump.to_str().unwrap(),
        ],
    );
    let target = common::TestDb::empty().await.unwrap();
    dr_script(
        "restore.sh",
        &[
            "--database-url",
            &target.url,
            "--in",
            dump.to_str().unwrap(),
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    {
        use numbering_migration::{Migrator, MigratorTrait};
        assert!(Migrator::get_pending_migrations(&target.conn)
            .await
            .unwrap()
            .is_empty());
        Migrator::up(&target.conn, None).await.unwrap();
    }

    // The restarted service: same provider (external state), restored database, two
    // worker instances at once.
    let h2 = Harness::build(Arc::new(target), None, t.h.clock.clone());
    let mut registry = numbering::providers::registry::ProviderRegistry::new(
        cf_service_auth::RuntimeEnvironment::Test,
    );
    registry.register(Arc::new(t.p.sim.clone())).unwrap();
    let commands = numbering::providers::commands::ProviderCommands::new(
        h2.numbers.clone(),
        Arc::new(registry),
        numbering::providers::commands::CommandSettings::default(),
    );
    let h2 = h2.with_providers(commands.clone());
    let steward = || {
        Stewardship::new(
            h2.numbers.clone(),
            Some(commands.clone()),
            Arc::new(EligibilityPolicy::default()),
            StewardshipSettings::default(),
        )
    };
    let (st1, st2) = (steward(), steward());
    let (c1, c2) = (ctx(), ctx());
    let (r1, r2) = (
        px::runner(&h2, &commands, "dr-worker-1"),
        px::runner(&h2, &commands, "dr-worker-2"),
    );
    tokio::join!(ax::drain(&r1), ax::drain(&r2));
    let (x, y) = tokio::join!(st1.advance_releases(&c1, 10), st2.advance_releases(&c2, 10));
    x.unwrap();
    y.unwrap();
    // Exactly one provider RELEASE across the restart and both workers.
    assert_eq!(releases(), 1);
    let r = st1.release_request(Some(&s), release.id).await.unwrap();
    assert_eq!(r.status, "QUARANTINED", "{r:?}");
    assert_eq!(
        h2.scalar(&format!(
            "SELECT count(*) FROM number_quarantines WHERE number_resource_id = '{number}'"
        ))
        .await,
        1
    );
    // The carrier's report replayed after the restart counts once.
    assert!(
        !st1.report_usage(&ctx(), "cfmeet.sms-gateway", usage)
            .await
            .unwrap()
            .1
    );
    assert_eq!(
        h2.scalar("SELECT count(*) FROM number_usage_records WHERE source_key = 'cfmeet.sms-gateway:sms-msg-dr-0001'")
            .await,
        1
    );
    // Quarantine completion after the cooldown: once, whichever worker wins.
    t.advance(Duration::days(8));
    let (x, y) = tokio::join!(
        st1.complete_quarantines(&c1, 10),
        st2.complete_quarantines(&c2, 10)
    );
    assert_eq!(x.unwrap() + y.unwrap(), 1);
    assert_eq!(
        h2.numbers.resource(number).await.unwrap().lifecycle_state(),
        S::Released
    );
    assert_eq!(releases(), 1);
}

// ---------------------------------------------------------------------------------------
// Capacity measurement (in-process, local PostgreSQL). Prints one `LOAD …` line per
// scenario and asserts correctness only; the numbers are this machine's, not a
// production capacity claim.
// ---------------------------------------------------------------------------------------

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn report(scenario: &str, concurrency: usize, mut ms: Vec<f64>, total: std::time::Duration) {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "LOAD numbering {scenario} n={} concurrency={concurrency} elapsed_ms={:.0} throughput_per_s={:.0} p50_ms={:.1} p95_ms={:.1} max_ms={:.1}",
        ms.len(),
        total.as_secs_f64() * 1000.0,
        ms.len() as f64 / total.as_secs_f64(),
        percentile(&ms, 0.50),
        percentile(&ms, 0.95),
        ms.last().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w_load_inbound_resolution_relay_lease_contention_and_usage_ingestion() {
    let Some(t) = setup().await else { return };
    let s = scope(EnvironmentKind::Sandbox);
    let (number, a) = t.active(&s).await;
    let e164 = t.e164(number).await;
    let t = Arc::new(t);
    const CONCURRENCY: usize = 16;

    // 1. Inbound voice resolutions of one ACTIVE DCN (each issues a revision-bound grant).
    let gate = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
    let started = std::time::Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..400 {
        let (t, gate, e164) = (t.clone(), gate.clone(), e164.clone());
        set.spawn(async move {
            let _p = gate.acquire_owned().await.unwrap();
            let at = std::time::Instant::now();
            let r = t.resolve(&e164).await;
            assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
            at.elapsed().as_secs_f64() * 1000.0
        });
    }
    let mut ms = Vec::new();
    while let Some(v) = set.join_next().await {
        ms.push(v.unwrap());
    }
    report("inbound_resolution", CONCURRENCY, ms, started.elapsed());

    // 2. Relay-lease contention: 20 relay numbers, 5 holders racing for each.
    let mut refs = Vec::new();
    for i in 0..20 {
        let m =
            t.st.import_relay_number(
                &ctx(),
                "cfmeet.privacy-relay",
                numbering::stewardship::relay::RelayImport {
                    relay_inventory_ref: Uuid::new_v4(),
                    e164: format!("+120255501{:02}", 50 + i),
                    country: "US".into(),
                    region: None,
                    capabilities: vec![C::VoiceInbound, C::VoiceOutbound],
                    binding_ref: format!("relay-binding-load-{i}"),
                    synthetic: true,
                },
            )
            .await
            .unwrap()
            .0;
        refs.push(m.relay_inventory_ref);
    }
    let started = std::time::Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for r in &refs {
        for _ in 0..5 {
            let (t, gate, r) = (t.clone(), gate.clone(), *r);
            set.spawn(async move {
                let _p = gate.acquire_owned().await.unwrap();
                let at = std::time::Instant::now();
                let granted =
                    t.st.lease_relay_number(
                        &ctx(),
                        "cfmeet.privacy-relay",
                        numbering::stewardship::relay::LeaseRequest {
                            relay_inventory_ref: r,
                            holder_ref: Uuid::new_v4(),
                            generation: 1,
                            expires_at: t.h.now() + Duration::days(1),
                            idempotency_key: format!("lease-{}", Uuid::new_v4().simple()),
                        },
                    )
                    .await
                    .is_ok();
                (granted, at.elapsed().as_secs_f64() * 1000.0)
            });
        }
    }
    let (mut ms, mut granted) = (Vec::new(), 0);
    while let Some(v) = set.join_next().await {
        let (g, m) = v.unwrap();
        granted += usize::from(g);
        ms.push(m);
    }
    report("relay_lease_contention", CONCURRENCY, ms, started.elapsed());
    assert_eq!(granted, 20, "exactly one lease per relay number");
    assert_eq!(
        t.h.scalar("SELECT count(*) FROM number_relay_leases WHERE status = 'ACTIVE'")
            .await,
        20
    );

    // 3. Carrier usage ingestion: 1000 reports, every one sent twice (duplicates collapse).
    let started = std::time::Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..2000 {
        let (t, gate, a) = (t.clone(), gate.clone(), a.id);
        set.spawn(async move {
            let _p = gate.acquire_owned().await.unwrap();
            let at = std::time::Instant::now();
            let now = t.h.now();
            t.st.report_usage(
                &ctx(),
                "cfmeet.sms-gateway",
                NewUsage {
                    source_key: format!("sms-load-{:04}", i % 1000),
                    number_resource_id: number,
                    assignment_id: Some(a),
                    kind: UsageKind::SmsInboundSegments,
                    quantity: 1,
                    provider_cost_minor: Some(1),
                    currency: Some("USD".into()),
                    period_start: now,
                    period_end: now,
                },
            )
            .await
            .unwrap();
            at.elapsed().as_secs_f64() * 1000.0
        });
    }
    let mut ms = Vec::new();
    while let Some(v) = set.join_next().await {
        ms.push(v.unwrap());
    }
    report("usage_ingestion", CONCURRENCY, ms, started.elapsed());
    assert_eq!(
        t.h.scalar(
            "SELECT count(*) FROM number_usage_records WHERE source_key LIKE 'cfmeet.sms-gateway:sms-load-%'"
        )
        .await,
        1000
    );
}
