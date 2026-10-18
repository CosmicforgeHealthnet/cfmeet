//! Part IV Workstream G: the NumberingProvider contract, the deterministic provider, durable
//! provider commands, fault injection, authenticated callbacks, read-back reconciliation,
//! the reusable certification suite — and the E–G integration paths (Step 17).
mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::Duration;
use common::{
    catalog, eligible_decision, exec, kyc as fx,
    providers::{self as px, Providers, CALLBACK_SECRET},
    scalar, scope, strings, voice_to_principal, Harness,
};
use numbering::{
    domain::{RequestContext, SharedClock},
    numbers::{
        assignments::NewAssignment,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::{EligibilityOutcome, SubjectKind},
        Actor, Capability as C, DomainError, EnvironmentKind, InventorySource, LifecycleState as S,
        NumberType, OwnerScope, OwnershipClass,
    },
    providers::{
        certification::{certify, CaseStatus, CertificationHarness},
        commands::{CommandStatus, ProviderCommands},
        deterministic::{DeterministicProvider, Fault},
        incidents,
        reconcile::Reconciled,
        registry::{ProviderRegistry, RegistryError},
        CallbackRequest, InventoryQuery, NumberingProvider, ProviderOperation as Op,
    },
};
use uuid::Uuid;

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn actor() -> Actor {
    common::actor()
}

fn us() -> InventoryQuery {
    InventoryQuery {
        country: "US".into(),
        number_type: None,
        capabilities: vec![],
        region: None,
        limit: 100,
    }
}

struct G {
    h: Harness,
    p: Providers,
}

async fn setup() -> Option<G> {
    let h = Harness::new().await?;
    let p = px::setup(&h);
    let h = h.with_providers(p.commands.clone());
    catalog::synthetic_us(&h).await;
    Some(G { h, p })
}

impl G {
    fn c(&self) -> &ProviderCommands {
        &self.p.commands
    }

    async fn drain(&self) {
        let runner = px::runner(&self.h, self.c(), "worker");
        for _ in 0..10 {
            let report = runner.run_due().await.unwrap();
            if report.claimed == 0 {
                break;
            }
        }
    }

    /// Delivers every pending provider callback; returns the dispositions.
    async fn deliver(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for cb in self.p.sim.take_callbacks() {
            out.push(
                self.c()
                    .ingest_callback(&ctx(), "synthetic", &cb)
                    .await
                    .unwrap()
                    .disposition,
            );
        }
        out
    }

    async fn ingest(&self) -> Vec<numbering::numbers::resources::ResourceRow> {
        self.c()
            .ingest_inventory(&ctx(), "synthetic", &us())
            .await
            .unwrap()
    }

    async fn resource_of(&self, external_ref: &str) -> Uuid {
        let id = strings(
            self.h.conn(),
            &format!("SELECT number_resource_id::text FROM number_provider_inventory_refs WHERE external_ref = '{external_ref}'"),
        )
        .await
        .pop()
        .unwrap_or_else(|| panic!("{external_ref} not ingested"));
        Uuid::parse_str(&id).unwrap()
    }

    async fn state(&self, id: Uuid) -> S {
        self.h.numbers.resource(id).await.unwrap().lifecycle_state()
    }

    /// Hold (through a command) → an assignment holding it with a routing profile.
    async fn reserved(
        &self,
        s: &OwnerScope,
        external_ref: &str,
    ) -> (Uuid, numbering::numbers::assignments::AssignmentRow) {
        let n = &self.h.numbers;
        let id = self.resource_of(external_ref).await;
        let (reservation, sub) = self
            .c()
            .request_hold(
                &ctx(),
                s,
                actor(),
                id,
                Duration::minutes(30),
                &format!("hold-{}", Uuid::new_v4().simple()),
            )
            .await
            .unwrap();
        assert!(!sub.replayed);
        self.drain().await;
        assert_eq!(
            self.c().command(sub.command.id).await.unwrap().status(),
            CommandStatus::Succeeded
        );
        assert_eq!(self.state(id).await, S::Reserved);
        let a = n
            .request_assignment(
                &ctx(),
                s,
                actor(),
                NewAssignment {
                    number_resource_id: id,
                    ownership_class: OwnershipClass::Campaign,
                    owner_ref: Uuid::new_v4(),
                    policy_snapshot_ref: None,
                    kyc_profile_ref: None,
                },
            )
            .await
            .unwrap();
        let a = n
            .submit_for_eligibility(&ctx(), actor(), a.id, a.revision)
            .await
            .unwrap();
        let a = n
            .record_eligibility(
                &ctx(),
                actor(),
                a.id,
                a.revision,
                eligible_decision(self.h.conn(), s, self.h.now()).await,
            )
            .await
            .unwrap();
        let a = n
            .attach_reservation(&ctx(), actor(), a.id, a.revision, reservation.id)
            .await
            .unwrap();
        let profile = n
            .create_routing_profile(&ctx(), s, actor(), voice_to_principal())
            .await
            .unwrap();
        let a = n
            .set_assignment_routing(&ctx(), actor(), a.id, a.revision, profile.profile_id)
            .await
            .unwrap();
        (id, a)
    }

    /// … → provisioned through a command (all routable capabilities) → ACTIVE.
    async fn active(
        &self,
        s: &OwnerScope,
        external_ref: &str,
        caps: &[C],
    ) -> (Uuid, numbering::numbers::assignments::AssignmentRow) {
        let (id, a) = self.reserved(s, external_ref).await;
        let sub = self
            .c()
            .request_provision(
                &ctx(),
                actor(),
                id,
                caps.to_vec(),
                &format!("prov-{}", Uuid::new_v4().simple()),
            )
            .await
            .unwrap();
        self.drain().await;
        let cmd = self.c().command(sub.command.id).await.unwrap();
        assert_eq!(cmd.status(), CommandStatus::Succeeded, "{:?}", cmd.result);
        assert_eq!(self.state(id).await, S::Provisioned);
        let n = &self.h.numbers;
        let a = n
            .begin_activation(&ctx(), actor(), a.id, a.revision)
            .await
            .unwrap();
        let a = n
            .activate_assignment(&ctx(), actor(), a.id, a.revision)
            .await
            .unwrap();
        assert_eq!(self.state(id).await, S::Active);
        (id, a)
    }

    async fn open_incidents(&self, category: &str) -> i64 {
        scalar(self.h.conn(), &format!("SELECT count(*) FROM numbering_incidents WHERE category = '{category}' AND status = 'OPEN'")).await
    }
}

// ---------------------------------------------------------------------------------------
// Step 16: the reusable certification suite
// ---------------------------------------------------------------------------------------

struct DeterministicHarness {
    sim: DeterministicProvider,
    clock: numbering::domain::ManualClock,
    destructive: bool,
}

impl CertificationHarness for DeterministicHarness {
    fn provider(&self) -> &dyn NumberingProvider {
        &self.sim
    }
    fn query(&self) -> InventoryQuery {
        us()
    }
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        use numbering::domain::Clock;
        self.clock.now()
    }
    fn destructive_allowed(&self) -> bool {
        self.destructive
    }
    fn inject(&self, op: Op, fault: Fault) -> bool {
        self.sim.inject(op, fault);
        true
    }
    fn set_outage(&self, outage: bool) -> bool {
        self.sim.set_outage(outage);
        true
    }
    fn drain_callbacks(&self) -> Vec<CallbackRequest> {
        self.sim.take_callbacks()
    }
    fn secret_markers(&self) -> Vec<String> {
        vec![String::from_utf8_lossy(CALLBACK_SECRET).to_string()]
    }
}

fn harness(destructive: bool) -> DeterministicHarness {
    let clock = numbering::domain::ManualClock::new(common::t0());
    let shared: SharedClock = Arc::new(clock.clone());
    DeterministicHarness {
        sim: DeterministicProvider::new(shared, CALLBACK_SECRET),
        clock,
        destructive,
    }
}

#[tokio::test]
async fn the_deterministic_adapter_passes_the_whole_certification_suite() {
    let report = certify(&harness(true)).await;
    assert!(report.failures().is_empty(), "{:#?}", report.failures());
    assert!(report.skipped().is_empty(), "{:#?}", report.skipped());
    assert_eq!(
        (report.passed(), report.cases.len()),
        (19, 19),
        "{report:#?}"
    );
    assert_eq!(report.provider_id, "synthetic");
}

#[tokio::test]
async fn destructive_and_live_cases_are_gated_never_reported_as_passed() {
    let report = certify(&harness(false)).await;
    assert!(report.failures().is_empty(), "{:#?}", report.failures());
    let skipped: Vec<_> = report.skipped().iter().map(|c| c.name).collect();
    for name in [
        "reservation_is_idempotent_and_exclusive",
        "release_is_final",
        "timeout_after_execution_is_unknown_and_read_back_shows_it",
    ] {
        assert!(
            skipped.contains(&name),
            "{name} must be skipped without opt-in"
        );
    }
    assert!(report
        .cases
        .iter()
        .any(|c| c.name == "inventory_is_normalized" && c.status == CaseStatus::Passed));
}

// ---------------------------------------------------------------------------------------
// Step 11 / 13: registry, environment restrictions, inventory
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_synthetic_provider_is_refused_in_production_and_never_serves_real_inventory() {
    let clock: SharedClock = Arc::new(numbering::domain::ManualClock::new(common::t0()));
    for runtime in [
        cf_service_auth::RuntimeEnvironment::Staging,
        cf_service_auth::RuntimeEnvironment::Production,
    ] {
        let mut r = ProviderRegistry::new(runtime);
        assert_eq!(
            r.register(Arc::new(DeterministicProvider::new(
                clock.clone(),
                CALLBACK_SECRET
            ))),
            Err(RegistryError::SyntheticInProtectedRuntime)
        );
    }
    let mut r = ProviderRegistry::new(cf_service_auth::RuntimeEnvironment::Local);
    r.register(Arc::new(DeterministicProvider::new(
        clock.clone(),
        CALLBACK_SECRET,
    )))
    .unwrap();
    assert!(matches!(
        r.register(Arc::new(DeterministicProvider::new(clock, CALLBACK_SECRET))),
        Err(RegistryError::Duplicate(_))
    ));
    assert!(r
        .for_inventory("synthetic", InventorySource::LiveProvider)
        .is_none());
    assert!(r
        .for_inventory("synthetic", InventorySource::SyntheticTest)
        .is_some());
    let d = r.get("synthetic").unwrap().descriptor();
    assert!(d.synthetic && !d.pstn_routable && !d.supports(Op::PortIn));

    // A command for real inventory never reaches the synthetic adapter.
    let Some(g) = setup().await else {
        return;
    };
    let real = g.h.available_number(InventorySource::LiveProvider).await;
    exec(
        g.h.conn(),
        &format!(
            "INSERT INTO number_provider_inventory_refs (id, number_resource_id, provider_id, external_ref, first_seen_at) \
             VALUES (gen_random_uuid(), '{}', 'synthetic', 'syn-voice-only', now())",
            real.id
        ),
    )
    .await
    .unwrap();
    let prod = scope(EnvironmentKind::Production);
    let (reservation, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &prod,
            actor(),
            real.id,
            Duration::minutes(30),
            "hold-real-0001",
        )
        .await
        .unwrap();
    g.drain().await;
    let cmd = g.c().command(sub.command.id).await.unwrap();
    assert_eq!(
        (cmd.status(), cmd.failure_code.as_deref()),
        (CommandStatus::Failed, Some("PROVIDER_NOT_REGISTERED"))
    );
    assert!(
        g.p.sim.calls().iter().all(|(op, _, _)| *op != Op::Reserve),
        "nothing was sent"
    );
    assert_eq!(
        g.h.numbers
            .reservation(&prod, reservation.id)
            .await
            .unwrap()
            .status,
        "FAILED"
    );
}

#[tokio::test]
async fn e_catalog_to_g_inventory_classifies_with_synthetic_provenance_and_f_requirements_apply() {
    let Some(g) = setup().await else {
        return;
    };
    let resources = g.ingest().await;
    assert_eq!(
        resources.len(),
        7,
        "restricted / unavailable inventory is not offered"
    );
    assert!(resources
        .iter()
        .all(|r| r.lifecycle_state() == S::Available && r.inventory_source == "SYNTHETIC_TEST"));
    // Stable identity: searching again maps to the same canonical ids.
    let again = g.ingest().await;
    let ids = |rs: &[numbering::numbers::resources::ResourceRow]| {
        rs.iter()
            .map(|r| r.id)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(ids(&resources), ids(&again));
    assert_eq!(
        scalar(g.h.conn(), "SELECT count(*) FROM number_resources").await,
        7
    );

    // Mobile: catalog + synthetic evidence → supported, never carrier-confirmed.
    let mobile = g.resource_of("syn-voice-sms-mobile").await;
    let r = g.h.numbers.resource(mobile).await.unwrap();
    assert_eq!(
        (r.number_type.as_str(), r.classification_source.as_str()),
        ("MOBILE", "SYNTHETIC")
    );
    let res = g.h.numbers.resolve_capabilities(mobile).await.unwrap();
    for c in [C::VoiceInbound, C::SmsOutbound, C::MobileNumberClass] {
        let e = res.get(c);
        assert!(e.supported, "{c}");
        assert!(e.provenance.synthetic && !e.provenance.carrier_confirmed);
    }
    // Inbound ≠ outbound; SMS ≠ mobile.
    let inbound =
        g.h.numbers
            .resolve_capabilities(g.resource_of("syn-inbound-only").await)
            .await
            .unwrap();
    assert!(inbound.get(C::VoiceInbound).supported && !inbound.get(C::VoiceOutbound).supported);
    let geo =
        g.h.numbers
            .resolve_capabilities(g.resource_of("syn-voice-sms-geo").await)
            .await
            .unwrap();
    assert!(geo.get(C::SmsInbound).supported && !geo.get(C::MobileNumberClass).supported);
    // A type the catalog does not cover fails closed whatever the provider offers.
    let national =
        g.h.numbers
            .resolve_capabilities(g.resource_of("syn-sms-only").await)
            .await
            .unwrap();
    assert!(!national.get(C::SmsInbound).supported);

    // F: the KYC-required offer carries the restriction; the requirements apply.
    fx::synthetic_us(&g.h).await;
    let kyc =
        g.h.numbers
            .resource(g.resource_of("syn-kyc-mobile").await)
            .await
            .unwrap();
    assert!(kyc.restriction_flags.contains(&"KYC_REQUIRED".to_string()));
    let s = scope(EnvironmentKind::Sandbox);
    let ask = |id: Uuid, ownership: OwnershipClass| EligibilityRequest {
        subject_kind: if ownership == OwnershipClass::Personal {
            SubjectKind::Individual
        } else {
            SubjectKind::Organization
        },
        subject_ref: Uuid::new_v4(),
        country: "US".into(),
        number_type: NumberType::Mobile,
        ownership_class: ownership,
        capabilities: vec![C::VoiceInbound],
        number_resource_id: Some(id),
        provider_id: Some("synthetic".into()),
    };
    let standing = Standing {
        active: true,
        first_party: false,
        live_telecom_allowed: false,
    };
    let d =
        g.h.numbers
            .evaluate_eligibility(
                &ctx(),
                &s,
                standing,
                &EligibilityPolicy::default(),
                ask(kyc.id, OwnershipClass::Personal),
            )
            .await
            .unwrap();
    assert_eq!(d.outcome(), EligibilityOutcome::KycRequired);
    // The generic synthetic policy requires no KYC, but the number's KYC_REQUIRED flag
    // must be covered: an uncovered flag fails closed.
    let d =
        g.h.numbers
            .evaluate_eligibility(
                &ctx(),
                &s,
                standing,
                &EligibilityPolicy::default(),
                ask(kyc.id, OwnershipClass::Campaign),
            )
            .await
            .unwrap();
    assert_eq!(d.outcome(), EligibilityOutcome::Denied);
    assert!(d
        .reasons
        .to_string()
        .contains("PROVIDER_REQUIREMENTS_UNMET"));
    let d =
        g.h.numbers
            .evaluate_eligibility(
                &ctx(),
                &s,
                standing,
                &EligibilityPolicy::default(),
                ask(mobile, OwnershipClass::Campaign),
            )
            .await
            .unwrap();
    assert_eq!(d.outcome(), EligibilityOutcome::Eligible);
}

// ---------------------------------------------------------------------------------------
// Step 12 / 13: durable commands over the whole simulated lifecycle
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_number_runs_its_whole_provider_lifecycle_through_durable_commands() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let caps = [
        C::VoiceInbound,
        C::VoiceOutbound,
        C::SmsInbound,
        C::SmsOutbound,
    ];
    let (id, a) = g.active(&s, "syn-voice-sms-mobile", &caps).await;
    // Binding evidence: the provider object, all routable capabilities; canonical id kept.
    let bindings = g.h.numbers.bindings(id).await.unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        (
            bindings[0].provider_id.as_str(),
            bindings[0].external_ref.as_str()
        ),
        ("synthetic", "syn-voice-sms-mobile")
    );
    assert_eq!(bindings[0].capabilities.len(), 4);
    // Routes, suspension, restoration.
    let voice = Uuid::new_v4();
    let sub = g
        .c()
        .request_route(&ctx(), id, true, voice, "route-voice-0001")
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Succeeded
    );
    let sms = g
        .c()
        .request_route(&ctx(), id, false, Uuid::new_v4(), "route-sms-00001")
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c().command(sms.command.id).await.unwrap().status(),
        CommandStatus::Succeeded
    );
    g.c()
        .request_suspend(&ctx(), id, "ABUSE_REVIEW", "suspend-00001")
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(g.state(id).await, S::Suspended);
    g.c()
        .request_restore(&ctx(), id, "restore-00001")
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(g.state(id).await, S::Active);
    match g.p.sim.inspect("syn-voice-sms-mobile").await {
        numbering::providers::ProviderOutcome::Succeeded(st) => {
            assert_eq!(st.voice_route, Some(voice));
            assert!(st.owned_by_us);
        }
        other => panic!("{other:?}"),
    }
    // Release: assignment first (D), then the provider release command.
    let n = &g.h.numbers;
    let a = n
        .request_release(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    let _a = n
        .remove_routing(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    assert_eq!(g.state(id).await, S::Releasing);
    let rel = g
        .c()
        .request_release(&ctx(), actor(), id, "release-00001")
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c().command(rel.command.id).await.unwrap().status(),
        CommandStatus::Succeeded
    );
    assert_eq!(g.state(id).await, S::Quarantined);
    assert!(g
        .h
        .numbers
        .bindings(id)
        .await
        .unwrap()
        .iter()
        .all(|b| b.status == "ENDED"));
    // Every command left attempt evidence and events; provider effects happened once.
    let commands = g.c().commands_for(id).await.unwrap();
    assert_eq!(commands.len(), 7);
    for c in &commands {
        let attempts = g.c().attempts(c.id).await.unwrap();
        assert_eq!(attempts.len(), 1, "{}", c.operation);
        assert!(attempts[0].transmitted && attempts[0].outcome == "SUCCEEDED");
        assert!(g.p.sim.executions(c.operation().provider_operation(), c.id) <= 1);
    }
    assert_eq!(
        scalar(g.h.conn(), &format!("SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.provider-command.changed' AND aggregate_id = '{id}'")).await,
        14,
        "requested + completed per command"
    );
    // The provider's callbacks confirm what the commands already applied.
    let dispositions = g.deliver().await;
    assert!(!dispositions.is_empty());
    assert!(
        dispositions
            .iter()
            .all(|d| matches!(*d, "CONFIRMED_EXISTING" | "IGNORED_TERMINAL")),
        "{dispositions:?}"
    );
    assert_eq!(
        g.state(id).await,
        S::Quarantined,
        "late callbacks never resurrect"
    );
}

#[tokio::test]
async fn commands_are_idempotent_exclusive_and_never_execute_twice() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let id = g.resource_of("syn-voice-only").await;
    let (r1, s1) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-key-00001",
        )
        .await
        .unwrap();
    let (r2, s2) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-key-00001",
        )
        .await
        .unwrap();
    assert!(s2.replayed && s1.command.id == s2.command.id && r1.id == r2.id);
    assert_eq!(
        scalar(g.h.conn(), "SELECT count(*) FROM number_reservations").await,
        1
    );
    // Another lifecycle command while one is in flight: refused.
    assert!(matches!(
        g.c()
            .request_hold(
                &ctx(),
                &s,
                actor(),
                id,
                Duration::minutes(30),
                "hold-key-00002"
            )
            .await
            .unwrap_err(),
        DomainError::Precondition { .. } | DomainError::Conflict { .. }
    ));
    // The same key for a different command: refused.
    assert!(matches!(
        g.c()
            .request_release(&ctx(), actor(), id, "hold-key-00001")
            .await
            .unwrap_err(),
        DomainError::Conflict {
            code: "IDEMPOTENCY_KEY_REUSED",
            ..
        }
    ));
    // Re-running the dispatch (duplicate job delivery) never re-executes.
    g.c().run_dispatch(&ctx(), s1.command.id).await.unwrap();
    g.c().run_dispatch(&ctx(), s1.command.id).await.unwrap();
    g.drain().await;
    assert_eq!(g.p.sim.executions(Op::Reserve, s1.command.id), 1);
    assert_eq!(g.c().attempts(s1.command.id).await.unwrap().len(), 1);
    // The database refuses tampering with a command's intent or a completed command.
    let cid = s1.command.id;
    for sql in [
        format!("UPDATE number_provider_commands SET request = '{{}}', revision = revision + 1 WHERE id = '{cid}'"),
        format!("UPDATE number_provider_commands SET status = 'PENDING', revision = revision + 1 WHERE id = '{cid}'"),
        format!("DELETE FROM number_provider_commands WHERE id = '{cid}'"),
        format!("UPDATE number_provider_command_attempts SET outcome = 'FAILED' WHERE command_id = '{cid}'"),
    ] {
        assert!(exec(g.h.conn(), &sql).await.is_err(), "{sql}");
    }
    // Secrets never enter commands.
    let err = exec(
        g.h.conn(),
        &format!(
            "INSERT INTO number_provider_commands (id, provider_id, operation, number_resource_id, idempotency_key, request, \
               status, max_attempts, correlation_id, created_at, updated_at) VALUES (gen_random_uuid(), 'synthetic', 'RELEASE', \
               '{id}', 'secret-test-01', '{{\"api_key\": \"x\"}}', 'PENDING', 5, 'corr_abcdefgh', now(), now())"
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("ck_number_provider_commands_request"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------
// Step 14: deterministic failure injection
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_timeout_before_transmission_is_retried_and_succeeds_once() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let id = g.resource_of("syn-voice-only").await;
    g.p.sim
        .inject(Op::Reserve, Fault::TimeoutBeforeTransmission);
    let (_, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-retry-001",
        )
        .await
        .unwrap();
    g.drain().await;
    let cmd = g.c().command(sub.command.id).await.unwrap();
    assert_eq!(
        (cmd.status(), cmd.attempts),
        (CommandStatus::Pending, 1),
        "definite, nothing sent: retry"
    );
    assert_eq!(g.state(id).await, S::Reserving);
    g.h.clock.advance(Duration::seconds(5));
    g.drain().await;
    let cmd = g.c().command(sub.command.id).await.unwrap();
    assert_eq!((cmd.status(), cmd.attempts), (CommandStatus::Succeeded, 2));
    let attempts = g.c().attempts(cmd.id).await.unwrap();
    assert_eq!(
        attempts
            .iter()
            .map(|a| (a.outcome.as_str(), a.transmitted))
            .collect::<Vec<_>>(),
        [("NOT_TRANSMITTED", false), ("SUCCEEDED", true)]
    );
    assert_eq!(g.p.sim.executions(Op::Reserve, cmd.id), 1);
    assert_eq!(g.state(id).await, S::Reserved);
}

#[tokio::test]
async fn a_timeout_after_execution_is_unknown_never_resent_and_reconciled_from_read_back() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let (id, _a) = g.reserved(&s, "syn-voice-sms-geo").await;
    g.p.sim.inject(Op::Provision, Fault::TimeoutAfterExecution);
    let sub = g
        .c()
        .request_provision(
            &ctx(),
            actor(),
            id,
            vec![C::VoiceInbound, C::VoiceOutbound, C::SmsInbound],
            "prov-unknown-1",
        )
        .await
        .unwrap();
    g.drain().await;
    let cmd = g.c().command(sub.command.id).await.unwrap();
    assert_eq!(cmd.status(), CommandStatus::Unknown);
    let r = g.h.numbers.resource(id).await.unwrap();
    assert_eq!(
        (r.lifecycle_state(), r.uncertain_operation.as_deref()),
        (S::UnknownProviderState, Some("PROVISION"))
    );
    assert_eq!(g.open_incidents("UNKNOWN_PROVIDER_OUTCOME").await, 1);
    assert!(
        g.h.numbers.bindings(id).await.unwrap().is_empty(),
        "no premature final state"
    );
    // Running the workers again does not resend (reconciliation is not due yet).
    g.drain().await;
    assert_eq!(
        g.p.sim
            .calls()
            .iter()
            .filter(|(op, r, _)| *op == Op::Provision && *r == Some(cmd.id))
            .count(),
        1
    );
    // Due: read-back shows it happened → reconciled success with evidence.
    g.h.clock.advance(Duration::seconds(31));
    g.drain().await;
    let cmd = g.c().command(cmd.id).await.unwrap();
    assert_eq!(cmd.status(), CommandStatus::ReconciledSucceeded);
    assert_eq!(g.state(id).await, S::Provisioned);
    assert_eq!(g.h.numbers.bindings(id).await.unwrap().len(), 1);
    assert_eq!(
        g.open_incidents("UNKNOWN_PROVIDER_OUTCOME").await,
        0,
        "resolved by reconciliation"
    );
    let attempts = g.c().attempts(cmd.id).await.unwrap();
    assert_eq!(
        attempts
            .iter()
            .map(|a| a.outcome.as_str())
            .collect::<Vec<_>>(),
        ["UNKNOWN", "READ_BACK"]
    );
    assert_eq!(g.p.sim.executions(Op::Provision, cmd.id), 1);
    let history = g.h.numbers.lifecycle_events(id).await.unwrap();
    assert_eq!(history.last().unwrap().actor_kind, "RECONCILIATION");
}

#[tokio::test]
async fn a_lost_request_reconciles_to_not_happened_and_reverts_the_intent() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let id = g.resource_of("syn-toll-free").await;
    g.p.sim.inject(Op::Reserve, Fault::LostRequest);
    let (reservation, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-lost-0001",
        )
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    assert_eq!(g.state(id).await, S::UnknownProviderState);
    assert_eq!(
        g.h.numbers
            .reservation(&s, reservation.id)
            .await
            .unwrap()
            .status,
        "UNKNOWN"
    );
    // Provider outage during reconciliation: still unknown, nothing assumed.
    g.p.sim.set_outage(true);
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::Pending
    );
    assert_eq!(g.state(id).await, S::UnknownProviderState);
    assert_eq!(g.open_incidents("PROVIDER_OUTAGE").await, 1);
    g.p.sim.set_outage(false);
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::DidNotHappen
    );
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::ReconciledFailed
    );
    assert_eq!(g.state(id).await, S::Available);
    assert_eq!(
        g.h.numbers
            .reservation(&s, reservation.id)
            .await
            .unwrap()
            .status,
        "FAILED"
    );
    assert_eq!(g.p.sim.executions(Op::Reserve, sub.command.id), 0);
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::AlreadyFinal
    );
}

#[tokio::test]
async fn definite_provider_refusals_revert_the_intent_with_their_classification() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    for (i, (fault, code)) in [
        (Fault::AlreadyOwned, "ALREADY_OWNED"),
        (Fault::ReservationConflict, "RESERVATION_CONFLICT"),
        (Fault::InventoryDisappears, "NOT_FOUND"),
    ]
    .into_iter()
    .enumerate()
    {
        let ext = ["syn-inbound-only", "syn-voice-only", "syn-toll-free"][i];
        let id = g.resource_of(ext).await;
        g.p.sim.inject(Op::Reserve, fault);
        let (reservation, sub) = g
            .c()
            .request_hold(
                &ctx(),
                &s,
                actor(),
                id,
                Duration::minutes(30),
                &format!("hold-def-{i:05}"),
            )
            .await
            .unwrap();
        g.drain().await;
        let cmd = g.c().command(sub.command.id).await.unwrap();
        assert_eq!(
            (cmd.status(), cmd.failure_code.as_deref()),
            (CommandStatus::Failed, Some(code))
        );
        assert_eq!(
            g.h.numbers
                .reservation(&s, reservation.id)
                .await
                .unwrap()
                .status,
            "FAILED"
        );
        assert_eq!(g.state(id).await, S::Available, "{code}");
    }
    // A refused provisioning returns the number to RESERVED.
    let (id, _) = g.reserved(&s, "syn-voice-sms-mobile").await;
    g.p.sim
        .inject(Op::Provision, Fault::Reject("PROVISIONING_REJECTED".into()));
    let sub = g
        .c()
        .request_provision(
            &ctx(),
            actor(),
            id,
            vec![C::VoiceInbound],
            "prov-rejected-1",
        )
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c()
            .command(sub.command.id)
            .await
            .unwrap()
            .failure_code
            .as_deref(),
        Some("PROVISIONING_REJECTED")
    );
    assert_eq!(g.state(id).await, S::Reserved);
    // Unsupported optional operation.
    let port = g
        .c()
        .request_port_in(&ctx(), id, "losing-carrier-1", "port-in-00001")
        .await
        .unwrap();
    g.drain().await;
    let port = g.c().command(port.command.id).await.unwrap();
    assert_eq!(
        (port.status(), port.failure_code.as_deref()),
        (CommandStatus::Unsupported, Some("OPERATION_UNSUPPORTED"))
    );
    // Capability the binding does not serve: normalized refusal, no command.
    let (gid, _) = g
        .active(&s, "syn-voice-only", &[C::VoiceInbound, C::VoiceOutbound])
        .await;
    assert!(matches!(
        g.c()
            .request_route(&ctx(), gid, false, Uuid::new_v4(), "route-sms-none1")
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "CAPABILITY_NOT_SUPPORTED",
            ..
        }
    ));
}

#[tokio::test]
async fn partial_provisioning_release_unknown_and_drift_are_fail_closed_with_incidents() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    // Partial provisioning: what exists is bound; the gap is a drift incident.
    let (id, _) = g.reserved(&s, "syn-voice-sms-mobile").await;
    g.p.sim.inject(Op::Provision, Fault::PartialProvision);
    let sub = g
        .c()
        .request_provision(
            &ctx(),
            actor(),
            id,
            vec![C::VoiceInbound, C::SmsInbound],
            "prov-partial-1",
        )
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::Happened
    );
    let b = g.h.numbers.bindings(id).await.unwrap();
    assert_eq!(b[0].capabilities, vec!["VOICE_INBOUND".to_string()]);
    assert_eq!(g.open_incidents("PROVIDER_STATE_DRIFT").await, 1);

    // Drift: our hold request was lost and someone else holds the number.
    let other = g.resource_of("syn-inbound-only").await;
    g.p.sim.inject(Op::Reserve, Fault::LostRequest);
    let (_, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            other,
            Duration::minutes(30),
            "hold-drift-001",
        )
        .await
        .unwrap();
    g.drain().await;
    let intruder = numbering::providers::OperationRequest {
        request_id: Uuid::new_v4(),
        external_ref: "syn-inbound-only".into(),
        operation: numbering::providers::OperationKind::Reserve { hold_seconds: 600 },
    };
    assert!(matches!(
        g.p.sim.reserve(&intruder).await,
        numbering::providers::ProviderOutcome::Succeeded(_)
    ));
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::Drift
    );
    assert_eq!(
        g.state(other).await,
        S::UnknownProviderState,
        "stays fail-closed"
    );
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    assert_eq!(g.open_incidents("PROVIDER_STATE_DRIFT").await, 2);

    // Release outcome unknown → read-back shows released → quarantined.
    let (aid, a) = g
        .active(&s, "syn-voice-only", &[C::VoiceInbound, C::VoiceOutbound])
        .await;
    let n = &g.h.numbers;
    let a = n
        .request_release(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    n.remove_routing(&ctx(), actor(), a.id, a.revision)
        .await
        .unwrap();
    g.p.sim.inject(Op::Release, Fault::TimeoutAfterExecution);
    let rel = g
        .c()
        .request_release(&ctx(), actor(), aid, "release-unk-01")
        .await
        .unwrap();
    g.drain().await;
    let r = g.h.numbers.resource(aid).await.unwrap();
    assert_eq!(
        (r.lifecycle_state(), r.uncertain_operation.as_deref()),
        (S::UnknownProviderState, Some("RELEASE"))
    );
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), rel.command.id)
            .await
            .unwrap(),
        Reconciled::Happened
    );
    assert_eq!(g.state(aid).await, S::Quarantined);
    assert!(g
        .h
        .numbers
        .bindings(aid)
        .await
        .unwrap()
        .iter()
        .all(|b| b.status == "ENDED"));
}

#[tokio::test]
async fn a_process_interruption_mid_dispatch_is_recovered_by_reconciliation_not_retry() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let id = g.resource_of("syn-voice-only").await;
    let (reservation, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-crash-001",
        )
        .await
        .unwrap();
    // The worker claims the job, commits DISPATCHING, then dies (no answer recorded).
    let runner = px::runner(&g.h, g.c(), "doomed");
    let claimed = runner.claim(Uuid::new_v4()).await.unwrap();
    assert_eq!(claimed.len(), 1);
    exec(
        g.h.conn(),
        &format!("UPDATE number_provider_commands SET status = 'DISPATCHING', attempts = 1, revision = revision + 1 WHERE id = '{}'", sub.command.id),
    )
    .await
    .unwrap();
    // Lease expiry: the provider-affecting job is parked UNKNOWN_OUTCOME, never retried.
    g.h.clock.advance(Duration::seconds(31));
    let restarted = px::runner(&g.h, g.c(), "restarted");
    let report = restarted.run_due().await.unwrap();
    assert_eq!(report.parked_unknown, 1);
    assert_eq!(g.p.sim.executions(Op::Reserve, sub.command.id), 0);
    // Recovery: the command becomes UNKNOWN; reconciliation decides from read-back.
    assert_eq!(g.c().recover_interrupted(&ctx(), 10).await.unwrap(), 1);
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    assert_eq!(g.state(id).await, S::UnknownProviderState);
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::DidNotHappen
    );
    assert_eq!(g.state(id).await, S::Available);
    assert_eq!(
        g.h.numbers
            .reservation(&s, reservation.id)
            .await
            .unwrap()
            .status,
        "FAILED"
    );
    let job_status = strings(
        g.h.conn(),
        &format!(
            "SELECT status::text FROM numbering_jobs WHERE id = '{}'",
            sub.command.job_id.unwrap()
        ),
    )
    .await;
    assert_eq!(
        job_status,
        ["FAILED"],
        "the parked job is resolved by reconciliation"
    );
    // A re-delivered dispatch of a DISPATCHING command also goes to reconciliation.
    let other = g.resource_of("syn-inbound-only").await;
    let (_, sub2) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            other,
            Duration::minutes(30),
            "hold-crash-002",
        )
        .await
        .unwrap();
    exec(
        g.h.conn(),
        &format!("UPDATE number_provider_commands SET status = 'DISPATCHING', attempts = 1, revision = revision + 1 WHERE id = '{}'", sub2.command.id),
    )
    .await
    .unwrap();
    g.c().run_dispatch(&ctx(), sub2.command.id).await.unwrap();
    assert_eq!(
        g.c().command(sub2.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    assert_eq!(g.p.sim.executions(Op::Reserve, sub2.command.id), 0);
}

// ---------------------------------------------------------------------------------------
// Step 15: callbacks
// ---------------------------------------------------------------------------------------

async fn post_callback(
    g: &G,
    provider: &str,
    cb: &CallbackRequest,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/provider-callbacks/v1/{provider}"));
    for (k, v) in &cb.headers {
        builder = builder.header(k, v);
    }
    let reply =
        g.h.raw(builder.body(Body::from(cb.body.clone())).unwrap())
            .await;
    (reply.status, reply.body)
}

#[tokio::test]
async fn callbacks_are_authenticated_deduplicated_correlated_and_order_safe() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    // An uncertain hold; the provider's HOLD_CONFIRMED arrives through the real route.
    let id = g.resource_of("syn-voice-only").await;
    g.p.sim.inject(Op::Reserve, Fault::TimeoutAfterExecution);
    let (_, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-cb-00001",
        )
        .await
        .unwrap();
    g.drain().await;
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    let callbacks = g.p.sim.take_callbacks();
    assert_eq!(callbacks.len(), 1);
    let cb = &callbacks[0];
    // Tampered / unsigned / replayed / unknown provider: refused, nothing stored.
    let mut tampered = cb.clone();
    tampered.body = String::from_utf8(cb.body.clone())
        .unwrap()
        .replace("HOLD_CONFIRMED", "RELEASED")
        .into_bytes();
    let (status, body) = post_callback(&g, "synthetic", &tampered).await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("AUTHENTICATION_FAILED"))
    );
    let mut unsigned = cb.clone();
    unsigned
        .headers
        .retain(|(k, _)| !k.eq_ignore_ascii_case("CF-Webhook-Signature"));
    assert_eq!(
        post_callback(&g, "synthetic", &unsigned).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_callback(&g, "carrierx", cb).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        scalar(g.h.conn(), "SELECT count(*) FROM number_provider_events").await,
        0
    );
    assert_eq!(g.open_incidents("CALLBACK_AUTHENTICATION_FAILED").await, 1);
    // Authentic: stored once, correlated, reconciliation requested immediately.
    let (status, body) = post_callback(&g, "synthetic", cb).await;
    assert_eq!(
        (
            status,
            body["disposition"].as_str(),
            body["duplicate"].as_bool()
        ),
        (StatusCode::OK, Some("RECONCILE_REQUESTED"), Some(false))
    );
    let (status, body) = post_callback(&g, "synthetic", cb).await;
    assert_eq!(
        (status, body["duplicate"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    assert_eq!(
        scalar(g.h.conn(), "SELECT count(*) FROM number_provider_events").await,
        1
    );
    let event = g.c().provider_events(id).await.unwrap().pop().unwrap();
    assert_eq!(
        (event.command_id, event.event_kind.as_str()),
        (Some(sub.command.id), "HOLD_CONFIRMED")
    );
    // The callback alone does not finalize: the read-back does (scheduled now).
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::Unknown
    );
    g.drain().await;
    assert_eq!(
        g.c().command(sub.command.id).await.unwrap().status(),
        CommandStatus::ReconciledSucceeded
    );
    assert_eq!(g.state(id).await, S::Reserved);
    // A replay outside the window is refused even with a valid signature.
    g.h.clock.advance(Duration::minutes(10));
    let replay = g.p.sim.sign(
        &cb.body,
        (g.h.now() - Duration::minutes(10)).timestamp(),
        "redelivery-1",
        &event.provider_event_id,
    );
    assert_eq!(
        post_callback(&g, "synthetic", &replay).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Events are append-only.
    assert!(exec(
        g.h.conn(),
        "UPDATE number_provider_events SET disposition = 'APPLIED'"
    )
    .await
    .is_err());
    // Unmatched objects are recorded and raise an incident.
    let ghost = format!(
        r#"{{"eventId":"synev-ghost-1","kind":"PROVISIONED","externalRef":"syn-ghost","requestId":null,"sequence":9,"occurredAt":"{}","capabilities":null}}"#,
        g.h.now().to_rfc3339()
    );
    let ghost = g.p.sim.sign(
        ghost.as_bytes(),
        g.h.now().timestamp(),
        "dl-ghost-1",
        "synev-ghost-1",
    );
    assert_eq!(
        post_callback(&g, "synthetic", &ghost).await.1["disposition"],
        "UNMATCHED"
    );
    assert_eq!(g.open_incidents("UNMATCHED_PROVIDER_EVENT").await, 1);
}

#[tokio::test]
async fn g_capability_downgrades_update_e_immediately_and_stale_events_never_win() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let caps = [
        C::VoiceInbound,
        C::VoiceOutbound,
        C::SmsInbound,
        C::SmsOutbound,
    ];
    let (id, _) = g.active(&s, "syn-voice-sms-mobile", &caps).await;
    g.p.sim.take_callbacks();
    let before = g.h.numbers.resource(id).await.unwrap().capability_revision;
    assert!(
        g.h.numbers
            .resolve_capabilities(id)
            .await
            .unwrap()
            .get(C::SmsOutbound)
            .usable
    );
    // Two provider changes; the newer one is delivered first, the older one late.
    g.p.sim.change_capabilities(
        "syn-voice-sms-mobile",
        &[
            C::VoiceInbound,
            C::VoiceOutbound,
            C::SmsInbound,
            C::MobileNumberClass,
        ],
    );
    g.h.clock.advance(Duration::seconds(1));
    g.p.sim.change_capabilities(
        "syn-voice-sms-mobile",
        &[C::VoiceInbound, C::VoiceOutbound, C::MobileNumberClass],
    );
    let mut cbs = g.p.sim.take_callbacks();
    assert_eq!(cbs.len(), 2);
    let newer = cbs.pop().unwrap();
    let older = cbs.pop().unwrap();
    assert_eq!(
        g.c()
            .ingest_callback(&ctx(), "synthetic", &newer)
            .await
            .unwrap()
            .disposition,
        "APPLIED"
    );
    assert_eq!(
        g.c()
            .ingest_callback(&ctx(), "synthetic", &older)
            .await
            .unwrap()
            .disposition,
        "STALE"
    );
    let res = g.h.numbers.resolve_capabilities(id).await.unwrap();
    assert!(
        !res.get(C::SmsOutbound).supported && !res.get(C::SmsInbound).supported,
        "downgrade effective at once"
    );
    assert!(
        res.get(C::VoiceInbound).usable,
        "untouched capabilities stay"
    );
    assert!(res.capability_revision > before);
    assert!(
        scalar(g.h.conn(), &format!("SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.capability.changed' AND aggregate_id = '{id}' AND payload->>'status' = 'WITHDRAWN'")).await >= 2
    );
    // A stale read-back is not authoritative either.
    let other = g.resource_of("syn-inbound-only").await;
    g.p.sim.inject(Op::Reserve, Fault::TimeoutAfterExecution);
    let (_, sub) = g
        .c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            other,
            Duration::minutes(30),
            "hold-stale-001",
        )
        .await
        .unwrap();
    g.drain().await;
    g.deliver().await; // HOLD_CONFIRMED at the new provider version
    g.p.sim.inject(Op::Inspect, Fault::StaleReadBack);
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::Pending
    );
    assert_eq!(g.state(other).await, S::UnknownProviderState);
    assert_eq!(
        g.c()
            .reconcile_command(&ctx(), sub.command.id)
            .await
            .unwrap(),
        Reconciled::Happened
    );
}

#[tokio::test]
async fn released_numbers_are_never_resurrected_and_unsolicited_releases_fail_closed() {
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let (id, _) = g
        .active(
            &s,
            "syn-voice-sms-geo",
            &[C::VoiceInbound, C::VoiceOutbound, C::SmsInbound],
        )
        .await;
    g.p.sim.take_callbacks();
    // The provider takes the number away: drift, incident, nothing changes locally.
    g.p.sim.revoke("syn-voice-sms-geo");
    assert_eq!(g.deliver().await, ["CONFLICT"]);
    assert_eq!(
        g.state(id).await,
        S::Active,
        "fail closed: never silently released"
    );
    assert_eq!(g.open_incidents("PROVIDER_RELEASED_UNEXPECTEDLY").await, 1);
    // Binding replacement keeps the canonical CF Meet id (Step 17).
    let binding = g.h.numbers.bindings(id).await.unwrap().pop().unwrap();
    let replaced =
        g.h.numbers
            .replace_binding(
                &ctx(),
                binding.id,
                numbering::numbers::bindings::NewBinding {
                    provider_id: "synthetic".into(),
                    external_ref: "syn-voice-sms-geo-ported".into(),
                    capabilities: vec![C::VoiceInbound, C::VoiceOutbound, C::SmsInbound],
                    change_reason: numbering::numbers::BindingChangeReason::ProviderReplacement,
                },
            )
            .await
            .unwrap();
    assert_eq!(replaced.number_resource_id, id);
    assert_eq!(
        g.c().provider_object_of(id).await.unwrap(),
        (
            "synthetic".to_string(),
            "syn-voice-sms-geo-ported".to_string()
        )
    );
    assert_eq!(g.h.numbers.resource(id).await.unwrap().id, id);
}

#[tokio::test]
async fn the_provider_migration_refuses_to_drop_in_flight_commands_and_reapplies() {
    use numbering_migration::{Migrator, MigratorTrait};
    let Some(g) = setup().await else {
        return;
    };
    g.ingest().await;
    let s = scope(EnvironmentKind::Sandbox);
    let id = g.resource_of("syn-voice-only").await;
    g.c()
        .request_hold(
            &ctx(),
            &s,
            actor(),
            id,
            Duration::minutes(30),
            "hold-migr-0001",
        )
        .await
        .unwrap();
    let steps = common::steps_down_to("m20261019_000005_provider_commands");
    assert!(Migrator::down(g.h.conn(), Some(steps)).await.is_err());
    let Some(fresh) = common::TestDb::empty().await else {
        return;
    };
    Migrator::up(&fresh.conn, None).await.unwrap();
    Migrator::down(&fresh.conn, Some(steps)).await.unwrap();
    let q = "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND (table_name LIKE 'number_provider_%' \
             AND table_name <> 'number_provider_bindings' OR table_name = 'numbering_incidents')";
    assert_eq!(scalar(&fresh.conn, q).await, 0);
    Migrator::up(&fresh.conn, None).await.unwrap();
    assert_eq!(scalar(&fresh.conn, q).await, 5);
    let _ = incidents::list(&fresh.conn, 10).await.unwrap();
}
